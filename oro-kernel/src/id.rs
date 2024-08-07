//! Implements Oro IDs in the kernel.
#![allow(clippy::module_name_repetitions)]

use core::{marker::ConstParamTy, str::FromStr};

/// An Oro ID.
///
/// IDs are globally unique IDs for various objects in the Oro ecosystem;
/// namely, modules and port types.
///
/// They are 128-bit values, and are formatted as `$T-[0-9AC-HJKMNPQRT-Z-]{25}`,
/// where `$T` is a 3-bit type identifier. The type identifier is used to
/// differentiate between different types of IDs; most values are reserved.
/// A type value of `0` is invalid.
///
/// The remaining 125 bits (25 characters) are treated as random. It is only
/// a requirement of the kernel that, for each type, unique values are used
/// for individual objects. The kernel does not care about the actual value
/// of the ID, and will not try to introspect it.
///
/// All type values other than 0 are reserved by the Oro ecosystem; usage of
/// a type value other than those defined by the Oro ecosystem is undefined
/// and **highly discouraged**.
///
/// # Human Representation
/// IDs are represented in human-readable form as 27-character strings,
/// whereby the type identifier is the first character, followed by
/// a hypen (`-`), and then a 25 character, 5-bit base32-encoded value.
///
/// The value is encoded from the character class `0-9AC-HJKMNPQRT-Z-`, which
/// contains 32 characters total to represent each of the 5-bit values.
///
/// Further, the encoding is case-insensitive. It's also human-tolerant,
/// whereby `B` is read as `8`, `S` is read as `5`, `I` and `L` are read
/// as `1`, and `O` is read as `0`. Further, `_` is read as `-`.
///
/// This is to prevent confusion between similar characters, especially in
/// cases where they are communicated verbally or in handwriting.
///
/// For consistency, all IDs _should_ be rendered in upper-case, with numbers
/// as opposed to the exceptional letter analogs (e.g. the number `5` instead of
/// the letter `S`).
///
/// # Bit Representation
/// The byte array is octal-based; there is no byte-wise endianness concern.
/// The bits within bytes are traversed starting with the most significant bit;
/// this means the type ID - a 3 bit value - is stored in the first byte's
/// most significant bits: bits 7, 6, and 5.
///
/// The remaining 125 bits are pulled from the remaining 15 bytes, in similar
/// fashion, packed as 5-bit values. Thus, the first digit of the base32-encoded
/// ID value is in the first byte's bits 4:0, the second digit is
/// in the second byte's bits 7:3, the third digit is in the second
/// byte's bits 2:0, and then continuing in the third byte's 7:6, and so on -
/// the last digit being in the last byte's bits 4:0.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Id<const TY: IdType>([u8; 16]);

/// Represents an unknown type ID.
///
/// This is particularly useful when parsing user-provided IDs
/// where the type is not known until parsing.
///
/// For more information on the ID format, see [`Id`].
pub struct AnyId([u8; 16]);

/// An ID type.
///
/// These are the only valid ID types in the Oro ecosystem.
/// Their designators are stable and will never change.
/// The ID type `0` is reserved and will never be valid.
/// All other ID types are reserved for the Oro ecosystem
/// and **should not** be used for any purpose (and will be
/// rejected by the kernel for all operations).
#[derive(Debug, PartialEq, Eq, Copy, Clone, ConstParamTy)]
#[non_exhaustive]
#[repr(u8)]
pub enum IdType {
	/// A module ID.
	Module   = 1,
	/// A port type ID.
	PortType = 2,
}

#[allow(clippy::missing_docs_in_private_items)]
macro_rules! try_from_impl {
	($docs:literal, $ty:ty, $name:ident, $src:ident) => {
		#[doc = $docs]
		pub const fn $name(v: $ty) -> Option<Self> {
			const MODULE_ID: $ty = IdType::Module.$src();
			const PORT_TYPE_ID: $ty = IdType::PortType.$src();

			match v {
				MODULE_ID => Some(Self::Module),
				PORT_TYPE_ID => Some(Self::PortType),
				_ => None,
			}
		}
	};
}

impl IdType {
	try_from_impl!(
		"Tries to convert a raw `u8` value to an ID type.",
		u8,
		try_from_u8,
		id_u8
	);

	try_from_impl!(
		"Tries to convert a human-readable `char` value to an ID type.",
		char,
		try_from_char,
		id_char
	);

	try_from_impl!(
		"Tries to convert a human-readable byte to an ID type.\n\n**NOTE:** This is **not** the \
		 same as [`IdType::try_from_u8()`], despite accepting the same type.",
		u8,
		try_from_bchar,
		id_bchar
	);

	/// Returns the type identifier as a u8.
	pub const fn id_u8(self) -> u8 {
		self as u8
	}

	/// Returns the type identifier as a char.
	pub const fn id_char(self) -> char {
		match self {
			Self::Module => 'M',
			Self::PortType => 'P',
			#[allow(unreachable_patterns)]
			_ => '?',
		}
	}

	/// Returns the type identifier as a `b'...'` byte.
	///
	/// **NOTE:** This is _not_ the same thing as
	/// [`IdType::id_u8()`], which returns the raw value,
	/// despite returning the same type.
	pub const fn id_bchar(self) -> u8 {
		self.id_char() as u8
	}
}

impl<const TY: IdType> Id<TY> {
	/// Creates a new ID from a 16-byte array.
	///
	/// # Safety
	/// The most significant 3 bits of the first byte
	/// must be the type identifier. This is unchecked
	/// by this method, and must be verified by the caller.
	///
	/// Not doing so will result in undefined behavior in
	/// the kernel.
	pub unsafe fn new_unchecked(data: [u8; 16]) -> Self {
		debug_assert!((data[0] >> 5) == TY.id_u8(), "ID type mismatch");
		Self(data)
	}

	/// Creates a new ID from a 16-byte array.
	///
	/// The type identifier is overwritten with the
	/// provided type `T`.
	pub fn new(data: [u8; 16]) -> Self {
		let mut id = Self(data);
		id.0[0] &= 0b0001_1111;
		id.0[0] |= (TY as u8) << 5;
		id
	}

	/// Tries to create a new ID from a 16-byte array.
	///
	/// If the type does not match `Ty`, returns `None`.
	pub fn try_new(data: [u8; 16]) -> Option<Self> {
		if (data[0] >> 5) == (TY as u8) {
			Some(Self(data))
		} else {
			None
		}
	}

	/// Formats the ID as a string, mutating the buffer
	/// in-place and returning a `&str` slice.
	pub fn to_str<'a>(&self, buf: &'a mut [u8; 27]) -> &'a str {
		// SAFETY(qix-): This is safe since we've guaranteed
		// SAFETY(qix-): that the type is valid.
		unsafe { AnyId::to_str_unchecked(&self.0, buf) }
	}

	/// Returns a reference to the raw byte array.
	pub fn as_bytes(&self) -> &[u8; 16] {
		&self.0
	}
}

impl AnyId {
	/// Creates a new ID from a 16-byte array.
	///
	/// Note that the type identifier is not checked,
	/// though this is still marked as safe. Conversion
	/// to a usable `Id` must be performed via the `try_into`
	/// method.
	pub fn new(data: [u8; 16]) -> Self {
		Self(data)
	}

	/// Returns the type of the ID.
	///
	/// If the ID contains an invalid type,
	/// returns `None`.
	pub fn ty(&self) -> Option<IdType> {
		IdType::try_from_u8(self.0[0] >> 5)
	}

	/// Tries to format the ID as an array of characters.
	///
	/// If the ID has an invalid type, returns `None`.
	///
	/// The buffer is mutated in-place, and then returned
	/// as a string slice.
	pub fn try_to_str<'a>(&self, buf: &'a mut [u8; 27]) -> Option<&'a str> {
		IdType::try_from_u8(self.0[0] >> 5)?;
		Some(unsafe { Self::to_str_unchecked(&self.0, buf) })
	}

	/// Formats the ID as a string, mutating the buffer
	/// in-place and returning a `&str` slice.
	///
	/// # Safety
	/// This method is unsafe because it does not check
	/// the type identifier of the ID. The caller must
	/// ensure that the type is valid before calling this
	/// method.
	///
	/// Calling this method with invalid type bytes may result
	/// in undefined behavior.
	pub unsafe fn to_str_unchecked<'a>(src: &[u8; 16], buf: &'a mut [u8; 27]) -> &'a str {
		#[allow(clippy::missing_docs_in_private_items)]
		const BASE32: [u8; 32] = *b"0123456789ACDEFGHJKMNPQRTUVWXYZ-";

		let ty: IdType = core::mem::transmute(src[0] >> 5);

		buf[0] = ty.id_bchar();
		buf[1] = b'-';

		// SAFETY(qix-): This assumes that the character encoding is
		// SAFETY(qix-): <= 8 bits (thus a single value is never going
		// SAFETY(qix-): to span more than 2 bytes). This is true for us
		// SAFETY(qix-): since each character is 5 bits encoded.
		for i in 0..25 {
			let bit_offset: u8 = (i * 5) + 3;
			let b0_index = bit_offset >> 3; // bit_offset / 8
			let b0_start = 8 - (bit_offset % 8);
			let b0_end = b0_start.saturating_sub(5);
			let b0_total = b0_start - b0_end;
			let b0_mask = (1 << b0_total) - 1;
			let b0 = (src[usize::from(b0_index)] >> b0_end) & b0_mask;

			let char_byte = if b0_total < 5 {
				let b1_index = b0_index + 1;
				// SAFETY(qix-): We can eschew the saturating sub
				// SAFETY(qix-): since we know that b1_end will never
				// SAFETY(qix-): hit the LSB, since the encoding is
				// SAFETY(qix-): 5 bits maximum.
				let b1_total = 5 - b0_total;
				let b1_end = 8 - b1_total;
				let b1_mask = (1 << b1_total) - 1;
				let b1 = (src[usize::from(b1_index)] >> b1_end) & b1_mask;

				let b = b0 << b1_total | b1;
				BASE32[usize::from(b)]
			} else {
				BASE32[usize::from(b0)]
			};

			buf[usize::from(i + 2)] = char_byte;
		}

		// SAFETY(qix-): the buffer is guaranteed to be the correct length
		// SAFETY(qix-): and is filled with valid characters.
		unsafe { core::str::from_utf8_unchecked(buf.as_slice()) }
	}

	/// Returns a reference to the raw byte array.
	///
	/// # Safety
	/// The caller must ensure that the type identifier
	/// is valid before using this method.
	pub unsafe fn as_bytes(&self) -> &[u8; 16] {
		&self.0
	}
}

/// Returned by `from_str()` when parsing fails.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ParseIdError {
	/// The ID has an invalid type identifier.
	///
	/// For [`AnyId`], this is returned when the type
	/// is invalid. For [`Id`], this is returned when
	/// the type is not the expected type.
	InvalidType,
	/// The ID is malformed (e.g. wrong length,
	/// missing hyphen, invalid characters, etc).
	///
	/// This is returned before `InvalidType` if the
	/// ID is malformed in any way, even if the
	/// type is invalid.
	Malformed,
}

fn try_to_buffer(s: &str) -> Result<[u8; 16], ParseIdError> {
	let s = s.as_bytes();

	if s.len() != 27 {
		return Err(ParseIdError::Malformed);
	}

	if s[1] != b'-' {
		return Err(ParseIdError::Malformed);
	}

	let ty = IdType::try_from_bchar(s[0]).ok_or(ParseIdError::Malformed)?;
	let ty_bits = ty.id_u8() << 5;

	let mut buf = [0; 16];

	buf[0] = ty_bits;

	for i in 0..25 {
		let ch = match s[i + 2] {
			c @ b'0'..=b'9' => c - b'0',
			b'o' | b'O' => 0,
			b'i' | b'I' | b'l' | b'L' => 1,
			b's' | b'S' => 5,
			b'b' | b'B' => 8,
			c @ b'A' => c - b'A' + 10,
			c @ b'a' => c - b'a' + 10,
			c @ b'C'..=b'H' => c - b'C' + 10 + (b'C' - b'A') - 1,
			c @ b'c'..=b'h' => c - b'c' + 10 + (b'c' - b'a') - 1,
			c @ b'J'..=b'K' => c - b'J' + 10 + (b'J' - b'A') - 2,
			c @ b'j'..=b'k' => c - b'j' + 10 + (b'j' - b'a') - 2,
			c @ b'M'..=b'N' => c - b'M' + 10 + (b'M' - b'A') - 3,
			c @ b'm'..=b'n' => c - b'm' + 10 + (b'm' - b'a') - 3,
			c @ b'P'..=b'R' => c - b'P' + 10 + (b'P' - b'A') - 4,
			c @ b'p'..=b'r' => c - b'p' + 10 + (b'p' - b'a') - 4,
			c @ b'T'..=b'Z' => c - b'T' + 10 + (b'T' - b'A') - 5,
			c @ b't'..=b'z' => c - b't' + 10 + (b't' - b'a') - 5,
			b'-' | b'_' => 31,
			_ => return Err(ParseIdError::Malformed),
		};

		debug_assert!(ch < 32, "invalid character encoding");

		let bit_offset = (i * 5) + 3;
		let b0_index = bit_offset >> 3; // bit_offset / 8
		let b0_start = 8 - (bit_offset % 8);
		let b0_end = b0_start.saturating_sub(5);
		let b0_total = b0_start - b0_end;

		let b1_total = 5 - b0_total;

		let b0 = ch >> b1_total;
		let b0 = b0 << b0_end;
		buf[b0_index] |= b0;

		if b1_total > 0 {
			let b1 = ch & ((1 << b1_total) - 1);
			let b1 = b1 << (8 - b1_total);
			buf[b0_index + 1] |= b1;
		}
	}

	Ok(buf)
}

impl FromStr for AnyId {
	type Err = ParseIdError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		try_to_buffer(s).map(Self::new)
	}
}

impl<const TY: IdType> FromStr for Id<TY> {
	type Err = ParseIdError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		try_to_buffer(s).and_then(|buf| {
			let ty = IdType::try_from_u8(buf[0] >> 5).ok_or(ParseIdError::InvalidType)?;

			if ty != TY {
				return Err(ParseIdError::InvalidType);
			}

			// SAFETY(qix-): We've already checked the type, so this is safe.
			Ok(unsafe { Self::new_unchecked(buf) })
		})
	}
}

impl<const TY: IdType> TryFrom<AnyId> for Id<TY> {
	type Error = ();

	fn try_from(value: AnyId) -> Result<Self, Self::Error> {
		if value.ty() == Some(TY) {
			// SAFETY(qix-): We've already checked the type, so this is safe.
			Ok(unsafe { Self::new_unchecked(value.0) })
		} else {
			Err(())
		}
	}
}

impl<const TY: IdType> From<Id<TY>> for AnyId {
	fn from(value: Id<TY>) -> Self {
		Self(value.0)
	}
}
