//! This module creates two sets of serialize and deserialize for bincode.
//! They explicitly spell out the bincode settings so that switching to
//! new versions in the future is less error prone.
//!
//! This is a list of all the row types and their settings for bincode.
//! +--------------+--------+------------+----------------+------------+
//! |              | Endian | Int Length | Allow Trailing | Byte Limit |
//! +--------------+--------+------------+----------------+------------+
//! | TxHistoryRow | big    | fixed      | allow          | unlimited  |
//! | All others   | little | fixed      | allow          | unlimited  |
//! +--------------+--------+------------+----------------+------------+
//!
//! Based on @junderw's https://github.com/mempool/electrs/pull/34. Thanks!

use bincode::Options;

pub fn serialize_big<T>(value: &T) -> Result<Vec<u8>, bincode::Error>
where
    T: ?Sized + serde::Serialize,
{
    big_endian().serialize(value)
}

pub fn deserialize_big<'a, T>(bytes: &'a [u8]) -> Result<T, bincode::Error>
where
    T: serde::Deserialize<'a>,
{
    big_endian().deserialize(bytes)
}

pub fn serialize_little<T>(value: &T) -> Result<Vec<u8>, bincode::Error>
where
    T: ?Sized + serde::Serialize,
{
    little_endian().serialize(value)
}

pub fn deserialize_little<'a, T>(bytes: &'a [u8]) -> Result<T, bincode::Error>
where
    T: serde::Deserialize<'a>,
{
    little_endian().deserialize(bytes)
}

/// This is the default settings for Options,
/// but all explicitly spelled out, except for endianness.
/// The following functions will add endianness.
#[inline]
fn options() -> impl Options {
    bincode::options()
        .with_fixint_encoding()
        .with_no_limit()
        .allow_trailing_bytes()
}

/// Adding the endian flag for big endian
#[inline]
fn big_endian() -> impl Options {
    options().with_big_endian()
}

/// Adding the endian flag for little endian
#[inline]
fn little_endian() -> impl Options {
    options().with_little_endian()
}
