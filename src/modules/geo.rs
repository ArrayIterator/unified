//! Geo: IP lookup, CIDR matching, classification, MaxMind database.

mod store;
pub mod cidr;
pub mod classification;
pub mod lookup;
mod db;
mod types;
mod handlers;
pub mod module;

pub use self::cidr::Cidr;
pub use self::cidr::ip_to_u128;
pub use self::cidr::u128_to_ip;
pub use self::cidr::normalize_ipv4;
pub use self::cidr::normalize_ip;
pub use self::classification::IpClass;
pub use self::classification::classify_ip;
pub use self::classification::is_bogon;
pub use self::classification::is_public_ip;
pub use self::classification::is_valid_maxmind_license_key;
pub use self::lookup::lookup_geo;
pub use self::lookup::find_override;
pub use self::lookup::lookup_maxmind;
pub use self::lookup::lookup_asn;
pub use self::module::GeoModule;
pub use self::module::register;
