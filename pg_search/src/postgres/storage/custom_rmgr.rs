// Copyright (c) 2023-2026 ParadeDB, Inc.
//
// This file is part of ParadeDB - Postgres for Search and Analytics
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use pgrx::{pg_guard, pg_sys};
use std::ffi::CStr;

// see https://wiki.postgresql.org/wiki/CustomWALResourceManagers
pub const RMGR_ID: pg_sys::RmgrId = 137;
const RMGR_NAME: &CStr = c"pg_search";

const XLOG_PG_SEARCH_INIT_INDEX: u8 = 0x10;
const XLOG_PG_SEARCH_INIT_INDEX_NAME: &CStr = c"INIT_INDEX";

/// Native PostgreSQL plans must not open ParadeDB metadata just to consider an
/// optional search path on a failover replica. Do not cache this: promotion can
/// happen while the backend (and its relcache) remains alive.
#[inline]
pub(crate) fn experimental_failover_in_recovery() -> bool {
    cfg!(feature = "experimental-physical-failover") && unsafe { pg_sys::RecoveryInProgress() }
}

#[pg_guard]
unsafe extern "C-unwind" fn rm_redo(record: *mut pg_sys::XLogReaderState) {
    let info = (*(*record).record).header.xl_info & !(pg_sys::XLR_INFO_MASK as u8);
    if info == XLOG_PG_SEARCH_INIT_INDEX
        && pg_sys::StandbyMode
        && !cfg!(feature = "experimental-physical-failover")
    {
        pgrx::error!(
            "replicas are not supported on community and require paradedb enterprise, \
             which guarantees physical replication safety on standbys."
        );
    }
    // INIT_INDEX is only a compatibility marker: ambuild WAL-logs the actual index
    // pages with log_newpage_range, and subsequent writes use full-page images or
    // Generic WAL. Keep recognizing old markers as well as emitting new ones so a
    // failover build can replay WAL produced before it was installed. This opt-in
    // does NOT make concurrent standby reads safe; MetaPage::open still rejects
    // them. Only access the replicated index once recovery has ended.
}

#[pg_guard]
unsafe extern "C-unwind" fn rm_desc(
    _buf: pg_sys::StringInfo,
    _record: *mut pg_sys::XLogReaderState,
) {
}

#[pg_guard]
unsafe extern "C-unwind" fn rm_identify(info: u8) -> *const ::core::ffi::c_char {
    match info & !(pg_sys::XLR_INFO_MASK as u8) {
        XLOG_PG_SEARCH_INIT_INDEX => XLOG_PG_SEARCH_INIT_INDEX_NAME.as_ptr(),
        _ => std::ptr::null(),
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn rm_startup() {}

#[pg_guard]
unsafe extern "C-unwind" fn rm_cleanup() {}

#[pg_guard]
unsafe extern "C-unwind" fn rm_mask(
    _pagedata: *mut ::core::ffi::c_char,
    _blkno: pg_sys::BlockNumber,
) {
}

pub fn emit_init_record() {
    // XLogRegisterData's signature varies across Postgres versions
    // (data is `*mut c_char` on pg15-17 vs `*const c_void` on pg18, and len is `c_int`
    // on pg15 vs `uint32` later). `as _` lets each build infer the right types.
    let mut payload: u8 = 0;
    unsafe {
        pg_sys::XLogBeginInsert();
        pg_sys::XLogRegisterData(&mut payload as *mut u8 as _, std::mem::size_of::<u8>() as _);
        pg_sys::XLogInsert(RMGR_ID, XLOG_PG_SEARCH_INIT_INDEX);
    }
}

pub fn register() {
    // `mut` because RegisterCustomRmgr takes `*mut RmgrData` on pg15 (vs `*const` later).
    // The function only reads the struct, but the C signature was tightened post-15.
    let mut rmgr = pg_sys::RmgrData {
        rm_name: RMGR_NAME.as_ptr(),
        rm_redo: Some(rm_redo),
        rm_desc: Some(rm_desc),
        rm_identify: Some(rm_identify),
        rm_startup: Some(rm_startup),
        rm_cleanup: Some(rm_cleanup),
        rm_mask: Some(rm_mask),
        // Postgres decode.c line 77: every record's xid needs to be processed by reorderbuffer
        // If this is set to none, Postgres will invoke reorderbuffer for us
        rm_decode: None,
    };

    unsafe {
        pg_sys::RegisterCustomRmgr(RMGR_ID, &mut rmgr as *mut _);
    }
}
