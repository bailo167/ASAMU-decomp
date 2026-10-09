//! Defensive reader for the Unreal Engine 3 package generation used by
//! *A Story About My Uncle* (package file version 868, licensee version 0).
//!
//! Status: not started. Every serialized offset and count is treated as
//! hostile input; the reader returns errors instead of panicking.
