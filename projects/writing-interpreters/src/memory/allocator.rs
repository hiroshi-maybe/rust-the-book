use std::{mem::size_of, ptr::NonNull};

use super::{
    stickyimmix::{BLOCK_CAPACITY, LINE_SIZE},
    AllocError, RawPtr,
};

// Object size ranges
const MAX_ALLOC_SIZE: usize = std::u32::MAX as usize;
const SMALL_OBJECT_MIN: usize = 1;
const SMALL_OBJECT_MAX: usize = LINE_SIZE;
const MEDIUM_OBJECT_MIN: usize = SMALL_OBJECT_MAX + 1;
const MEDIUM_OBJECT_MAX: usize = BLOCK_CAPACITY;
const LARGE_OBJECT_MIN: usize = MEDIUM_OBJECT_MAX + 1;
const LARGE_OBJECT_MAX: usize = MAX_ALLOC_SIZE;

/// A type that describes allocation of an object into a heap space, returning
/// a bare pointer type on success
pub trait AllocRaw {
    /// An implementation of an object header type
    type Header: AllocHeader;

    /// Allocate a single object of type T.
    fn alloc<T>(&self, object: T) -> Result<RawPtr<T>, AllocError>
    where
        T: AllocObject<<Self::Header as AllocHeader>::TypeId>;

    /// Allocating an array allows the client to put anything in the resulting data
    /// block but the type of the memory block will simply be 'Array'. No other
    /// type information will be stored in the object header.
    /// This is just a special case of alloc<T>() for T=u8 but a count > 1 of u8
    /// instances.  The caller is responsible for the content of the array.
    fn alloc_array(&self, size_bytes: ArraySize) -> Result<RawPtr<u8>, AllocError>;

    /// Given a bare pointer to an object, return the expected header address    
    fn get_header(object: NonNull<()>) -> NonNull<Self::Header>;

    /// Given a bare pointer to an object's header, return the expected object address
    fn get_object(header: NonNull<Self::Header>) -> NonNull<()>;
}

/// Object size class.
/// - Small objects fit inside a line
/// - Medium objects span more than one line
/// - Large objects span multiple blocks
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SizeClass {
    Small,
    Medium,
    Large,
}

impl SizeClass {
    pub fn get_for_size(object_size: usize) -> Result<SizeClass, AllocError> {
        match object_size {
            SMALL_OBJECT_MIN..=SMALL_OBJECT_MAX => Ok(SizeClass::Small),
            MEDIUM_OBJECT_MIN..=MEDIUM_OBJECT_MAX => Ok(SizeClass::Medium),
            LARGE_OBJECT_MIN..=LARGE_OBJECT_MAX => Ok(SizeClass::Large),
            _ => Err(AllocError::BadRequest),
        }
    }
}

/// Every object is `Allocated` on creation.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Mark {
    Allocated,
    Unmarked,
    Marked,
}

/// The type that describes the bounds of array sizing
pub type ArraySize = u32;
/// A managed-type type-identifier type should implement this!
pub trait AllocTypeId: Copy + Clone {}
/// All managed object types must implement this trait in order to be allocatable
pub trait AllocObject<T: AllocTypeId> {
    const TYPE_ID: T;
}

pub trait AllocHeader: Sized {
    /// Associated type that identifies the allocated object type
    type TypeId: AllocTypeId;

    /// Create a new header for object type O
    fn new<O: AllocObject<Self::TypeId>>(size: u32, size_class: SizeClass, mark: Mark) -> Self;

    /// Create a new header for an array type
    fn new_array(size: ArraySize, size_class: SizeClass, mark: Mark) -> Self;

    /// Set the Mark value to "marked"
    fn mark(&mut self);

    /// Get the current Mark value
    fn is_marked(&self) -> bool;

    /// Get the size class of the object
    fn size_class(&self) -> SizeClass;

    /// Get the size of the object in bytes
    fn size(&self) -> u32;

    /// Get the type of the object
    fn type_id(&self) -> Self::TypeId;
}

/// Return the allocated size of an object as it's size_of::<T>() value rounded
/// up to a double-word boundary
pub fn alloc_size_of(object_size: usize) -> usize {
    let align = size_of::<usize>();
    (object_size + align - 1) & !(align - 1)
}
