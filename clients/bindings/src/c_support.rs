//! Common C ownership and panic boundary. No C input points into Rust-owned data.
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};
pub struct TKBuffer {
    bytes: Vec<u8>,
}
pub(crate) fn buffer(bytes: Vec<u8>) -> *mut TKBuffer {
    Box::into_raw(Box::new(TKBuffer { bytes }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tidkod_buffer_free(value: *mut TKBuffer) {
    if !value.is_null() {
        unsafe {
            drop(Box::from_raw(value));
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tidkod_buffer_data(value: *const TKBuffer) -> *const u8 {
    unsafe { value.as_ref() }.map_or(ptr::null(), |v| v.bytes.as_ptr())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tidkod_buffer_len(value: *const TKBuffer) -> usize {
    unsafe { value.as_ref() }.map_or(0, |v| v.bytes.len())
}
pub(crate) unsafe fn input<'a>(data: *const u8, len: usize) -> Result<&'a [u8], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() || len > isize::MAX as usize {
        return Err("invalid byte span".into());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
fn float_span(data: *const f32, len: usize) -> Result<(), String> {
    if len > isize::MAX as usize / size_of::<f32>()
        || (len != 0 && (data.is_null() || !data.is_aligned()))
    {
        return Err("invalid float span".into());
    }
    Ok(())
}
// Caller owns the live allocation for the complete call. Output spans require
// exclusive access and must not alias handles, input spans, or result storage.
pub(crate) unsafe fn float_input<'a>(data: *const f32, len: usize) -> Result<&'a [f32], String> {
    float_span(data, len)?;
    if len == 0 {
        Ok(&[])
    } else {
        Ok(unsafe { std::slice::from_raw_parts(data, len) })
    }
}
pub(crate) unsafe fn float_output<'a>(data: *mut f32, len: usize) -> Result<&'a mut [f32], String> {
    float_span(data, len)?;
    if len == 0 {
        Ok(&mut [])
    } else {
        Ok(unsafe { std::slice::from_raw_parts_mut(data, len) })
    }
}
pub(crate) unsafe fn invoke(
    error: *mut *mut TKBuffer,
    f: impl FnOnce() -> Result<(), String>,
) -> i32 {
    if !error.is_null() {
        unsafe {
            error.write(ptr::null_mut());
        }
    }
    let result = catch_unwind(AssertUnwindSafe(f));
    let (status, message) = match result {
        Ok(Ok(())) => return 0,
        Ok(Err(e)) => (1, e),
        Err(_) => (2, "Rust panic in FFI operation".into()),
    };
    if !error.is_null() {
        unsafe {
            error.write(buffer(message.into_bytes()));
        }
    }
    status
}
