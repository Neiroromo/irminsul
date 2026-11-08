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
use crate::unk_util::{
    find_player_mora, find_player_nickname, find_player_props, find_player_uid, find_uid_beside_name,
    matches_avatar_fight_props,
    matches_avatar_prop_update, matches_gadget_interaction, matches_item_update, matches_player_location,
    matches_player_location_of,
    find_scene_arrival, find_teleport_target,
    matches_scene_gadgets, matches_talent_update,
};
pub use crate::unk_util::{QuestUpdate, StoryEntry, StoryStatus, AvatarMotion, AvatarPropUpdate, GadgetInteraction, PlayerLocation, SceneGadget, TalentUpdate};
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
    /// The client sent it (to the server), and did not receive it.
    pub from_client: bool,
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
            from_client: false,
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
    /// The session key kept from an earlier run fits: no new login is needed.
    SessionKeyResumed,
    /// The player id found in the server's answer to the token request.
    PlayerUid(u32),
    /// The user id the header of the packets carries, if it is not zero.
    HeaderUserId(u32),
    /// The player's name, found in the data sent when logging in.
    PlayerNickname(String),
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
            Self::SessionKeyResumed => write!(f, "session key kept from the earlier run fits: joined the running connection"),
            Self::PlayerUid(uid) => write!(f, "player id {uid} (from the token response)"),
            Self::HeaderUserId(uid) => write!(f, "packet header user id {uid}"),
            Self::PlayerNickname(name) => write!(f, "player name {name:?}"),
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
    nickname_found: bool,
    header_uid_seen: bool,
    /// The key search of this connection came up empty.
    key_search_failed: bool,
    samples_saved: u32,
    decrypt_failures: u32,
    last_event_flush: Option<Instant>,
    /// The session key of an earlier run of the program, to try when this one is
    /// started while the game is running (it is the same while the connection is).
    retained_key: Option<Vec<u8>>,
    /// The key in use came from that, and how many commands in a row it did not
    /// fit (the first may be a part of one that began before).
    key_resumed: bool,
    resumed_misses: u32,
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

    /// A session key from an earlier run, tried when the connection is joined in the
    /// middle.
    pub fn set_retained_key(mut self, key: Option<Vec<u8>>) -> Self {
        self.retained_key = key;
        self
    }

    /// The session key of the connection, once found.
    pub fn session_key(&self) -> Option<&[u8]> {
        match &self.key {
            Some(Key::Session(key)) => Some(key),
            _ => None,
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
                self.nickname_found = false;
                self.header_uid_seen = false;
                self.samples_saved = 0;
                self.recv_kcp = None;
                self.sent_kcp = None;
                self.key = None;
                self.key_resumed = false;
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
                .map(|mut command| {
                    command.from_client = direction == PacketDirection::Sent;
                    command
                })
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
                // The program was started while the game was running: the key of the
                // connection is the one kept, if it fits.
                if let Some(kept) = self.retained_key.clone() {
                    let mut test = data.clone();
                    decrypt_command(&kept, &mut test);
                    if test[0] == 0x45 && test[1] == 0x67 && test[test.len() - 2] == 0x89 && test[test.len() - 1] == 0xAB {
                        self.events.push(SnifferEvent::SessionKeyResumed);
                        self.key = Some(Key::Session(kept));
                        self.key_resumed = true;
                        self.resumed_misses = 0;
                        return self.receive_command(data);
                    }
                }
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
                    self.resumed_misses = 0;
                    self.key.as_ref().unwrap()
                } else if self.key_resumed && self.resumed_misses < 6 {
                    // The first things after joining may be pieces of what began before.
                    self.resumed_misses += 1;
                    return None;
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

        let split = (command.header_len as usize).min(command.proto_data.len());
        if !self.header_uid_seen
            && let Ok(head) = PacketHead::parse_from_bytes(&command.proto_data[..split])
            && head.user_id != 0
        {
            self.header_uid_seen = true;
            self.events.push(SnifferEvent::HeaderUserId(head.user_id));
        }
        if !self.nickname_found
            && command.data_len < 8000
            && let Some(name) = find_player_nickname(&command.proto_data[split..])
        {
            self.nickname_found = true;
            self.events.push(SnifferEvent::PlayerNickname(name));
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
                let split = (command.header_len as usize).min(command.proto_data.len());
                if let Some(uid) =
                    find_player_uid(&command.proto_data[split..], Some(header_command.sent_ms / 1000))
                {
                    self.events.push(SnifferEvent::PlayerUid(uid));
                }
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

/// The properties of a character that changed, if the command says so.
pub fn matches_avatar_props(game_command: &GameCommand) -> Option<AvatarPropUpdate> {
    matches_avatar_prop_update(&game_command.proto_data)
}

/// A talent that went up a level, if the command says so.
pub fn matches_talent_level(game_command: &GameCommand) -> Option<TalentUpdate> {
    matches_talent_update(&game_command.proto_data)
}

/// The stats of a character that changed, if the command says so.
pub fn matches_avatar_stats(game_command: &GameCommand) -> Option<(Vec<u64>, Vec<(u32, f32)>)> {
    matches_avatar_fight_props(&game_command.proto_data)
}

/// The stacks of materials that changed, if the command says so.
pub fn matches_item_changes(game_command: &GameCommand) -> Option<Vec<r#gen::protos::Item>> {
    matches_item_update(&game_command.proto_data)
}

/// All the player's properties, if the command is the message that has them.
pub fn matches_player_props(game_command: &GameCommand) -> Option<Vec<(u64, u64)>> {
    find_player_props(&game_command.proto_data)
}

/// The gadgets (chests among them) that came into view, if the command says so.
pub fn matches_gadgets_in_view(game_command: &GameCommand) -> Option<Vec<SceneGadget>> {
    matches_scene_gadgets(&game_command.proto_data)
}

/// What was done to a gadget of the world (a chest opened, say), if the command
/// is the game's answer to that.
pub fn matches_gadget_use(game_command: &GameCommand) -> Option<GadgetInteraction> {
    matches_gadget_interaction(&game_command.proto_data)
}

/// Where a teleport goes (the scene, and the number of the point in it), if the
/// command says that.  It is command 26942 in this version of the game.
pub fn matches_teleport_target(game_command: &GameCommand) -> Option<(u32, u32)> {
    if game_command.command_id != 26942 {
        return None;
    }
    find_teleport_target(&game_command.proto_data)
}

/// Where the player comes in when the scene changes (the scene, and the place),
/// if the command says that.  It is command 27748 in this version of the game.
pub fn matches_scene_arrival(game_command: &GameCommand) -> Option<(u32, [f32; 3])> {
    if game_command.command_id != 27748 {
        return None;
    }
    find_scene_arrival(&game_command.proto_data)
}

/// Where a player is, if the command says that (it does every few seconds).
/// `expected` is the id of the account that is followed, if it is known.
/// The state of quests, as the server says it (command 24303 in this version of the game):
/// the whole list when logging in, and what changed after.
pub fn matches_quest_updates(game_command: &GameCommand) -> Vec<QuestUpdate> {
    if game_command.command_id != 24303 || game_command.from_client {
        return Vec::new();
    }
    crate::unk_util::find_quest_updates(&game_command.proto_data)
}

/// The whole quest list, told when logging in (command 897 in this version of the game).
pub fn matches_quest_list(game_command: &GameCommand) -> Vec<StoryEntry> {
    if game_command.command_id != 897 || game_command.from_client {
        return Vec::new();
    }
    crate::unk_util::find_quest_list(&game_command.proto_data)
}

/// A story of the quest list and the state of each quest of it, as the server says it
/// (command 26691 in this version of the game).
pub fn matches_story_update(game_command: &GameCommand) -> Option<(u32, Vec<(u32, u32)>)> {
    if game_command.command_id != 26691 || game_command.from_client {
        return None;
    }
    crate::unk_util::find_story_update(&game_command.proto_data)
}

/// Where the player's avatar is and which way it faces, as the client tells the server as it
/// moves (several times a second), if the command has it.
pub fn matches_avatar_motion(game_command: &GameCommand) -> Option<AvatarMotion> {
    if !game_command.from_client || game_command.proto_data.len() > 6_000 {
        return None;
    }
    crate::unk_util::find_avatar_motion(&game_command.proto_data)
}

pub fn matches_location_of_player(game_command: &GameCommand, expected: Option<u32>) -> Option<PlayerLocation> {
    matches_player_location_of(&game_command.proto_data, expected)
}

/// The id of the player of this name, if the command has the name and the id
/// together.  For when the id was not in the answer to the login.
pub fn matches_uid_of_player(game_command: &GameCommand, name: &str) -> Option<u32> {
    find_uid_beside_name(&game_command.proto_data, name)
}

/// The player's mora, if the command has it.
pub fn matches_player_mora(game_command: &GameCommand) -> Option<u64> {
    find_player_mora(&game_command.proto_data)
}

/// Matches the full avatar list by content, whatever its command id is.
pub fn matches_avatar_packet(game_command: &GameCommand) -> Option<Vec<r#gen::protos::AvatarInfo>> {
    matches_avatars_all_data_notify(&game_command.proto_data)
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    /// A command as it is on the wire: magic, id, header and data lengths, data, tail,
    /// with the key put on it.
    fn on_the_wire(key: &[u8], command_id: u16, data: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x45, 0x67];
        bytes.extend_from_slice(&command_id.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&[0x89, 0xAB]);
        decrypt_command(key, &mut bytes);
        bytes
    }

    #[test]
    fn a_kept_session_key_that_fits_is_used_when_the_connection_is_joined() {
        let key: Vec<u8> = (0..4096u32).map(|n| (n * 7 + 3) as u8).collect();
        let mut sniffer = GameSniffer::new().set_retained_key(Some(key.clone()));
        // A piece of something that began before: nothing, and nothing is lost.
        assert!(sniffer.receive_command(vec![1; 40]).is_none());
        let command = sniffer.receive_command(on_the_wire(&key, 4469, b"abc")).expect("decrypted");
        assert_eq!(command.command_id, 4469);
        assert!(sniffer.session_key().is_some());
        // Pieces that do not fit a few times are borne; a good one goes on.
        assert!(sniffer.receive_command(vec![2; 40]).is_none());
        assert!(sniffer.receive_command(on_the_wire(&key, 20584, b"x")).is_some());
    }

    #[test]
    fn a_kept_session_key_that_does_not_fit_is_not_used() {
        let key: Vec<u8> = (0..4096u32).map(|n| (n * 7 + 3) as u8).collect();
        let other: Vec<u8> = (0..4096u32).map(|n| (n * 5 + 1) as u8).collect();
        let mut sniffer = GameSniffer::new().set_retained_key(Some(key));
        assert!(sniffer.receive_command(on_the_wire(&other, 4469, b"abc")).is_none());
        assert!(sniffer.session_key().is_none());
    }
}
