//! Bounded BitTorrent tracker protocol codecs shared by the tracker and probes.
use std::net::{IpAddr, SocketAddr};

pub mod bencode;
mod http;
mod udp;

pub use http::{
    encode_http_announce, encode_http_announce_query, encode_http_failure, encode_http_scrape,
    parse_http_announce, parse_http_scrape,
};
pub use udp::{
    decode_udp_announce_response, decode_udp_connect_response, encode_udp_announce,
    encode_udp_announce_request, encode_udp_connect, encode_udp_connect_request, encode_udp_error,
    encode_udp_scrape, parse_udp, udp_cookie, validate_udp_cookie,
};

pub type InfoHash = [u8; 20];
pub type PeerId = [u8; 20];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnnounceEvent {
    #[default]
    None,
    Started,
    Completed,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnounceRequest {
    pub info_hash: InfoHash,
    pub peer_id: PeerId,
    pub ip: IpAddr,
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: AnnounceEvent,
    pub numwant: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnounceResponse {
    pub interval: u32,
    pub min_interval: u32,
    pub complete: u32,
    pub incomplete: u32,
    pub peers: Vec<SocketAddr>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScrapeEntry {
    pub info_hash: InfoHash,
    pub complete: u32,
    pub downloaded: u32,
    pub incomplete: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UdpRequest {
    Connect {
        transaction_id: u32,
    },
    Announce {
        connection_id: u64,
        transaction_id: u32,
        request: AnnounceRequest,
    },
    Scrape {
        connection_id: u64,
        transaction_id: u32,
        hashes: Vec<InfoHash>,
    },
}
