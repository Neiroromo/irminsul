//! Parse network packets transmitted between the game and the server
//!
//! Packets are built up in following layers depending on the purpose of the packet:
//!
//! - Packets for connection management ([`GamePacket::Connection`])
//!     - **Ethernet/IP/UDP**, handled using [`etherparse`]
//!     - **[`ConnectionPacket`]**, containing events for connection establishment/disconnection
//! - Packets for game commands ([`GamePacket::Commands`])
//!     - **Ethernet/IP/UDP**, handled using [`etherparse`]
//!     - **KCP**, handled using [`kcp`]
//!         - The KCP header contains an extra field that needs to be removed
//!           to be compatible with the regular KCP protocol
//!     - **[`GameCommand`]**, encrypted using XOR
//!     - **Protobuf**, payload, needs to be parsed into using the types generated in [`gen::proto`]
//!
//! [`GameCommand`]s are encrypted using an XOR-key.
//! One of the first packets sent is a request for a new key from a seed.
//! That key is used for the rest of the packets.
//! This means the recording for packets needs to start before the game starts (train hyperdrive).
//!
//! ## Example
//! ```
//! use auto_artifactarium::{GamePacket, GameSniffer, ConnectionPacket};
//!
//! let packets: Vec<Vec<u8>> = vec![/**/];
//!
//! let mut sniffer = GameSniffer::new();
//! for packet in packets {
//!     match sniffer.receive_packet(packet) {
//!         Some(GamePacket::Connection(ConnectionPacket::Disconnected)) => {
//!             println!("Disconnected!");
//!             break;
//!         }
//!         Some(GamePacket::Commands(commands)) => {
//!             for command in commands {
//!                 println!("{:?}", command);
//!             }
//!         }
//!         _ => {}
//!     }
//! }
//! ```
//!

use std::collections::HashMap;
use std::fmt;
use std::fmt::Write;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use rsa::{RsaPrivateKey, pkcs1::DecodeRsaPrivateKey};
use tracing::{error, info, info_span, instrument, trace, warn};

use crate::connection::parse_connection_packet;
use crate::crypto::{bruteforce, decrypt_command, lookup_initial_key, retained_search};
// use crate::gen::protos::GetPlayerTokenRsp;
use crate::Key::Dispatch;
use crate::r#gen::protos::{PacketHead, Unk};
use protobuf::Message as _;
use protobuf::UnknownValueRef;
use crate::kcp::KcpSniffer;
pub use crate::unk_util::Achievement;
pub use crate::unk_util::{
    matches_achievement_all_data_notify, matches_avatars_all_data_notify,
    matches_get_player_token_rsp, matches_items_all_data_notify,
};

fn bytes_as_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, b| {
        let _ = write!(output, "{b:02x}");
        output
    })
}

// pub mod command_id;
pub mod r#gen;

mod connection;
mod crypto;
mod cs_rand;
mod kcp;
mod unk_util;

const PORTS: [u16; 2] = [22101, 22102];

/// Top-level packet sent by the game
pub enum GamePacket {
    Connection(ConnectionPacket),
    Commands(Vec<GameCommand>),
}

/// Packet for connection management
pub enum ConnectionPacket {
    HandshakeRequested,
    Disconnected,
    HandshakeEstablished,
    SegmentData(PacketDirection, Vec<u8>),
}

#[repr(u16)]
enum CommandId {
    AvatarDataNotify =  27799,
    PlayerStoreNotify = 22160,
}

/// Game command header.
///
/// Contains the type of the command in `command_id`
/// and the data encoded in protobuf in `proto_data`
///
/// ## Bit Layout
/// | Bit indices     |  Type |  Name |
/// | - | - | - |
/// |   0..2      |  `u16`  |  Header (magic constant) |
/// |   2..4      |  `u16`  |  command_id |
/// |   4..6      |  `u16`  |  header_len (unsure) |
/// |   6..10     |  `u32`  |  data_len |
/// |  10..10+data_len |  variable  |  proto_data |
/// | data_len..data_len+2  |  `u16`  |  Tail (magic constant) |
#[derive(Clone)]
pub struct GameCommand {
    pub command_id: u16,
    #[allow(unused)]
    pub header_len: u16,
    #[allow(unused)]
    pub data_len: u32,
    pub proto_data: Vec<u8>,
}

impl GameCommand {
    const HEADER_LEN: usize = 10;
    const TAIL_LEN: usize = 2;

    #[instrument(skip(bytes), fields(len = bytes.len()))]
    pub fn try_new(bytes: Vec<u8>) -> Option<Self> {
        let header_overhead = Self::HEADER_LEN + Self::TAIL_LEN;
        if bytes.len() < header_overhead {
            warn!(len = bytes.len(), "game command header incomplete");
            return None;
        }

        if bytes[0] != 0x45
            || bytes[1] != 0x67
            || bytes[bytes.len() - 2] != 0x89
            || bytes[bytes.len() - 1] != 0xAB
        {
            error!("Didn't get magic in try_new!");
            return None;
        }

        // skip header magic const
        let command_id = u16::from_be_bytes(bytes[2..4].try_into().unwrap());
        let header_len = u16::from_be_bytes(bytes[4..6].try_into().unwrap());
        let data_len = u32::from_be_bytes(bytes[6..10].try_into().unwrap());

        let data = bytes[10..10 + data_len as usize + header_len as usize].to_vec();
        Some(GameCommand {
            command_id,
            header_len,
            data_len,
            proto_data: data,
        })
    }

    pub fn parse_proto<T: protobuf::Message>(&self) -> protobuf::Result<T> {
        T::parse_from_bytes(&self.proto_data)
    }

    pub fn is_avatar_data_notify(&self) -> bool {
        self.command_id == CommandId::AvatarDataNotify as u16
    }

    pub fn is_player_store_notify(&self) -> bool {
        self.command_id == CommandId::PlayerStoreNotify as u16
    }
}

impl fmt::Debug for GameCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameCommand")
            .field("command_id", &self.command_id)
            .field("header_len", &self.header_len)
            .field("data_len", &self.data_len)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum PacketDirection {
    Sent,
    Received,
}

pub enum Key {
    Dispatch(Vec<u8>),
    Session(Vec<u8>),
}

/// Something the sniffer did or noticed, for diagnosing why no data comes in.
#[derive(Debug, Clone)]
pub enum SnifferEvent {
    HandshakeRequested,
    HandshakeEstablished,
    Disconnected,
    /// First segment of a connection direction was seen.
    ConnectionStarted {
        direction: PacketDirection,
        conv_id: u32,
    },
    /// Segments of a different connection than the one being followed.
    ForeignSegments {
        count: u32,
    },
    InitialKey {
        version: u16,
        found: bool,
    },
    /// The server's answer to the token request was understood.
    SeedsReceived {
        count: usize,
        sent_time: u64,
    },
    SessionKeyFound {
        /// The time the client's generator was seeded with, minus the time
        /// stamp of this connection's request, in ms.
        offset_ms: i64,
        /// Which number of that generator's sequence the key came from.
        output_index: usize,
    },
    /// No session key could be found for this connection; nothing in it can be
    /// decrypted.
    KeySearchFailed {
        seeds: usize,
        had_retained_seed: bool,
    },
    /// A command that could not be decrypted, kept for finding out why.
    UndecryptedSample(Vec<u8>),
    SessionKeyLost,
    /// The server's answer to the token request arrived, but the seed could not
    /// be read from it, so no session key can be derived.
    TokenResponseUnreadable {
        body_len: usize,
        /// Size the header declares for the uncompressed body, if it does.
        declared_size: Option<u64>,
        /// Whether the body parses as a protobuf message at all.
        looks_like_protobuf: bool,
    },
    /// Commands that could not be decrypted with any known key.
    DecryptFailed {
        count: u32,
    },
    /// A command with a large payload was decrypted.
    LargeCommand {
        command_id: u16,
        len: u32,
    },
}

impl fmt::Display for SnifferEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HandshakeRequested => write!(f, "handshake requested (new connection)"),
            Self::HandshakeEstablished => write!(f, "handshake packet"),
            Self::Disconnected => write!(f, "disconnect packet"),
            Self::ConnectionStarted { direction, conv_id } => {
                write!(f, "following connection {conv_id} ({direction:?})")
            }
            Self::ForeignSegments { count } => {
                write!(f, "{count} segments of another connection ignored")
            }
            Self::InitialKey { version, found } => match found {
                true => write!(f, "initial key found (key version {version})"),
                false => write!(f, "NO initial key for key version {version}"),
            },
            Self::SeedsReceived { count, sent_time } => {
                write!(f, "token response read: {count} seeds, time {sent_time}")
            }
            Self::SessionKeyFound {
                offset_ms,
                output_index,
            } => write!(
                f,
                "session key found (generator seeded {offset_ms:+} ms from this request, \
                 number {output_index} of its sequence)"
            ),
            Self::KeySearchFailed {
                seeds,
                had_retained_seed,
            } => write!(
                f,
                "KEY NOT FOUND: {seeds} seeds tried, earlier connection seen: {had_retained_seed}"
            ),
            Self::UndecryptedSample(bytes) => {
                write!(f, "saved a sample of {} undecryptable bytes", bytes.len())
            }
            Self::SessionKeyLost => write!(f, "session key stopped working"),
            Self::TokenResponseUnreadable {
                body_len,
                declared_size,
                looks_like_protobuf,
            } => write!(
                f,
                "token response NOT readable: {body_len} bytes, header declares \
                 uncompressed size {declared_size:?}, parses as protobuf: {looks_like_protobuf}"
            ),
            Self::DecryptFailed { count } => {
                write!(f, "{count} commands could not be decrypted")
            }
            Self::LargeCommand { command_id, len } => {
                write!(f, "command {command_id}, {len} bytes")
            }
        }
    }
}

#[derive(Default)]
pub struct GameSniffer {
    sent_kcp: Option<KcpSniffer>,
    recv_kcp: Option<KcpSniffer>,
    client_seed: Option<u64>,
    key: Option<Key>,
    initial_keys: HashMap<u16, Vec<u8>>,
    rsa_keys: Vec<RsaPrivateKey>,
    sent_time: Option<u64>,
    possible_seeds: Vec<u64>,
    events: Vec<SnifferEvent>,
    /// Whether the token response of the current connection was read.
    seeds_in_connection: bool,
    /// The key search of this connection came up empty.
    key_search_failed: bool,
    samples_saved: u32,
    decrypt_failures: u32,
    last_event_flush: Option<Instant>,
}

impl GameSniffer {
    pub fn new() -> Self {
        let pem_data_4 = include_str!("../keys/private_key_4.pem");
        let pem_data_5 = include_str!("../keys/private_key_5.pem");

        let rsa_4 = RsaPrivateKey::from_pkcs1_pem(pem_data_4);
        let rsa_5 = RsaPrivateKey::from_pkcs1_pem(pem_data_5);

        GameSniffer {
            rsa_keys: vec![rsa_4, rsa_5]
                .iter()
                .filter_map(|rsa_key| rsa_key.clone().ok())
                .collect(),
            ..Default::default()
        }
    }

    pub fn set_initial_keys(mut self, initial_keys: HashMap<u16, Vec<u8>>) -> Self {
        self.initial_keys = initial_keys;
        self
    }

    /// Returns what happened since the last call.  Repeating problems are
    /// summed up and reported at most once a second.
    pub fn take_events(&mut self) -> Vec<SnifferEvent> {
        let due = self
            .last_event_flush
            .is_none_or(|flushed| flushed.elapsed() >= Duration::from_secs(1));
        if due {
            self.last_event_flush = Some(Instant::now());
            let foreign: u32 = [&mut self.sent_kcp, &mut self.recv_kcp]
                .into_iter()
                .flatten()
                .map(|kcp| kcp.take_mismatches())
                .sum();
            if foreign > 0 {
                self.events.push(SnifferEvent::ForeignSegments { count: foreign });
            }
            let count = std::mem::take(&mut self.decrypt_failures);
            if count > 0 {
                self.events.push(SnifferEvent::DecryptFailed { count });
            }
        }
        std::mem::take(&mut self.events)
    }

    #[instrument(skip_all, fields(len = bytes.len()))]
    pub fn receive_packet(&mut self, bytes: Vec<u8>) -> Option<GamePacket> {
        let packet = parse_connection_packet(&PORTS, bytes)?;
        match packet {
            ConnectionPacket::HandshakeRequested => {
                info!("handshake requested, resetting state");
                self.events.push(SnifferEvent::HandshakeRequested);
                self.seeds_in_connection = false;
                self.key_search_failed = false;
                self.samples_saved = 0;
                self.recv_kcp = None;
                self.sent_kcp = None;
                self.key = None;
                Some(GamePacket::Connection(packet))
            }
            ConnectionPacket::HandshakeEstablished => {
                self.events.push(SnifferEvent::HandshakeEstablished);
                Some(GamePacket::Connection(packet))
            }
            ConnectionPacket::Disconnected => {
                self.events.push(SnifferEvent::Disconnected);
                Some(GamePacket::Connection(packet))
            }

            ConnectionPacket::SegmentData(direction, kcp_seg) => {
                let commands = self.receive_kcp_segment(direction, &kcp_seg);
                match commands {
                    Some(commands) => Some(GamePacket::Commands(commands)),
                    None => Some(GamePacket::Connection(ConnectionPacket::SegmentData(
                        direction, kcp_seg,
                    ))),
                }
            }
        }
    }

    fn receive_kcp_segment(
        &mut self,
        direction: PacketDirection,
        kcp_seg: &[u8],
    ) -> Option<Vec<GameCommand>> {
        let kcp = match direction {
            PacketDirection::Sent => &mut self.sent_kcp,
            PacketDirection::Received => &mut self.recv_kcp,
        };

        if kcp.is_none() {
            let new_kcp = KcpSniffer::try_new(kcp_seg)?;
            self.events.push(SnifferEvent::ConnectionStarted {
                direction,
                conv_id: new_kcp.conv_id(),
            });
            *kcp = Some(new_kcp);
        }

        if let Some(kcp) = kcp {
            let commands = kcp
                .receive_segments(kcp_seg)
                .into_iter()
                .filter_map(|data| self.receive_command(data))
                .collect();

            return Some(commands);
        }

        None
    }

    #[instrument(skip_all, fields(len = data.len()))]
    fn receive_command(&mut self, mut data: Vec<u8>) -> Option<GameCommand> {
        // Too short to be a command, and the checks below index into it.
        if data.len() < GameCommand::HEADER_LEN + GameCommand::TAIL_LEN {
            return None;
        }

        let key_r = match &self.key {
            None => {
                let key = lookup_initial_key(&self.initial_keys, &data);
                let version = u16::from_be_bytes([data[0], data[1]]) ^ 0x4567;
                self.events.push(SnifferEvent::InitialKey {
                    version,
                    found: key.is_some(),
                });
                match key {
                    Some(key) => {
                        self.key = Some(Dispatch(key));
                        self.key.as_ref().unwrap()
                    }
                    None => {
                        error!("No dispatch key found");
                        return None;
                    }
                }
            }
            Some(Dispatch(k)) => {
                let mut test = data.clone();
                decrypt_command(k, &mut test);

                if test[0] == 0x45
                    && test[1] == 0x67
                    && test[test.len() - 2] == 0x89
                    && test[test.len() - 1] == 0xAB
                {
                    self.key.as_ref().unwrap()
                } else {
                    if self.key_search_failed {
                        // Same seeds, same outcome: don't spend the time again.
                        self.decrypt_failures += 1;
                        return None;
                    }

                    let sent_time = self.sent_time.unwrap_or_default();
                    let mut found = None;
                    for &seed in &self.possible_seeds {
                        // An earlier connection of this game session tells when
                        // the client's generator was seeded, and later
                        // connections use later numbers of it.
                        if let Some(seed_time) = self.client_seed
                            && let Some(key) = retained_search(seed_time, seed, &data)
                        {
                            found = Some(key);
                            break;
                        }

                        // Otherwise assume this is the first connection, which
                        // seeds the generator about when it asks for the token.
                        if let Some(key) = bruteforce(sent_time, seed, &data) {
                            found = Some(key);
                            break;
                        }
                    }

                    match found {
                        Some(found) => {
                            self.client_seed = Some(found.time);
                            self.events.push(SnifferEvent::SessionKeyFound {
                                offset_ms: found.time as i64 - sent_time as i64,
                                output_index: found.output_index,
                            });
                            self.key = Some(Key::Session(found.key));
                            self.key.as_ref().unwrap()
                        }
                        None => {
                            self.key_search_failed = true;
                            self.decrypt_failures += 1;
                            self.events.push(SnifferEvent::KeySearchFailed {
                                seeds: self.possible_seeds.len(),
                                had_retained_seed: self.client_seed.is_some(),
                            });
                            if self.samples_saved < 2 {
                                self.samples_saved += 1;
                                self.events
                                    .push(SnifferEvent::UndecryptedSample(data.clone()));
                            }
                            error!("Couldn't bruteforce from deduced keys");
                            return None;
                        }
                    }
                }
            }
            Some(Key::Session(k)) => {
                let mut test = data.clone();
                decrypt_command(k, &mut test);

                if test[0] == 0x45 && test[1] == 0x67 {
                    //|| test[test.len() - 2] == 0x89 && test[test.len() - 1] == 0xAB
                    self.key.as_ref().unwrap()
                } else {
                    warn!("Invalidated session key");
                    self.events.push(SnifferEvent::SessionKeyLost);
                    self.key = None;
                    error!("Session key dead, relaunch game");
                    return None;
                }
            }
        };

        let key = match key_r {
            Dispatch(k) | Key::Session(k) => k,
        };

        decrypt_command(key, &mut data);

        let command = GameCommand::try_new(data)?;
        if command.data_len >= 300 {
            self.events.push(SnifferEvent::LargeCommand {
                command_id: command.command_id,
                len: command.data_len,
            });
        }

        let span = info_span!("command", ?command);
        let _enter = span.enter();

        info!("received");
        trace!(data = BASE64_STANDARD.encode(&command.proto_data), "data");

        // if !matches!(
        //     command.command_id,
        //     command_id::GET_PLAYER_TOKEN_RSP | command_id::ACHIEVEMENT_ALL_DATA_NOTIFY
        // ) {
        //     return None;
        // }

        if let Some(Dispatch(_)) = self.key {
            if let Some(possible_seeds) =
                matches_get_player_token_rsp(command.proto_data.clone(), self.rsa_keys.clone())
            {
                self.possible_seeds = possible_seeds;
                self.seeds_in_connection = true;
                self.key_search_failed = false;
                info!(?self.possible_seeds, "setting new possible session seeds");
                let header_command = command.parse_proto::<PacketHead>().unwrap();
                self.sent_time = Some(header_command.sent_ms);
                self.events.push(SnifferEvent::SeedsReceived {
                    count: self.possible_seeds.len(),
                    sent_time: header_command.sent_ms,
                });
                info!(?self.sent_time, "setting new send time");
            } else if !self.seeds_in_connection && command.data_len >= 5000 {
                // The first large command of a connection is the token
                // response.  Say why it can't be used.
                self.events.push(describe_unreadable_token_response(&command));
            }
        }

        Some(command)
    }
}

/// Describes a token response the seed could not be read from, to tell a
/// compressed or otherwise changed body from a missing key.
fn describe_unreadable_token_response(command: &GameCommand) -> SnifferEvent {
    let split = (command.header_len as usize).min(command.proto_data.len());
    let (head, body) = command.proto_data.split_at(split);
    let declared_size = PacketHead::parse_from_bytes(head)
        .ok()
        .and_then(|head| match head.special_fields.unknown_fields().get(8) {
            Some(UnknownValueRef::Varint(size)) => Some(size),
            _ => None,
        });
    let looks_like_protobuf = Unk::parse_from_bytes(body)
        .map(|message| message.unknown_fields().iter().next().is_some())
        .unwrap_or(false);
    SnifferEvent::TokenResponseUnreadable {
        body_len: body.len(),
        declared_size,
        looks_like_protobuf,
    }
}

pub fn matches_achievement_packet(game_command: &GameCommand) -> Option<Vec<Achievement>> {
    return matches_achievement_all_data_notify(game_command.proto_data.clone());
}

/// Matches the full item list by content, whatever its command id is.
pub fn matches_item_packet(game_command: &GameCommand) -> Option<Vec<r#gen::protos::Item>> {
    matches_items_all_data_notify(&game_command.proto_data)
}

/// Matches the full avatar list by content, whatever its command id is.
pub fn matches_avatar_packet(game_command: &GameCommand) -> Option<Vec<r#gen::protos::AvatarInfo>> {
    matches_avatars_all_data_notify(&game_command.proto_data)
}
