//! Finding games on Usenet and fetching them: wanted games (from IGDB), Newznab indexers,
//! release matching, SABnzbd, and importing what finishes.

pub mod config;
pub mod engine;
pub mod newznab;
pub mod qbit;
pub mod release;
pub mod sab;
pub mod store;
pub mod torrent;
pub mod unpack;
