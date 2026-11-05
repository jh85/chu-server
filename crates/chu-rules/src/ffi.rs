//! `ffi` backend: libchushogi.a through the C ABI in vendor/chushogi/src/capi.h.
//! All `unsafe` in the workspace is in this file.

use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr::NonNull;

use crate::{sfen_shape_ok, Color, Error, GameEnd, Referee, Rules, Status};

#[repr(C)]
struct ChuPos {
    _private: [u8; 0],
}

const CHU_OK: c_int = 0;
const CHU_EILLEGAL: c_int = -1;
const CHU_ESYNTAX: c_int = -2;
const CHU_ESPACE: c_int = -3;
const CHU_SFEN_CAP: usize = 512;
const CHU_LEGAL_CAP: usize = 16384;

extern "C" {
    fn chu_new() -> *mut ChuPos;
    fn chu_free(pos: *mut ChuPos);
    fn chu_set_sfen(pos: *mut ChuPos, sfen: *const c_char) -> c_int;
    fn chu_set_rules(pos: *mut ChuPos, rules: c_int) -> c_int;
    fn chu_move(pos: *mut ChuPos, usi: *const c_char) -> c_int;
    fn chu_legal(pos: *const ChuPos, buf: *mut c_char, cap: c_int) -> c_int;
    fn chu_status(pos: *const ChuPos, winner: *mut c_int) -> c_int;
    fn chu_in_check(pos: *const ChuPos, side: c_int) -> c_int;
    fn chu_sfen(pos: *const ChuPos, buf: *mut c_char, cap: c_int) -> c_int;
    fn chu_ply(pos: *const ChuPos) -> c_int;
}

pub struct FfiReferee {
    pos: NonNull<ChuPos>,
}

// The C++ position has no thread affinity; it is only ever used through
// `&mut self` or `&self`, so moving it between threads is sound.
unsafe impl Send for FfiReferee {}

impl Drop for FfiReferee {
    fn drop(&mut self) {
        unsafe { chu_free(self.pos.as_ptr()) }
    }
}

fn code(rc: c_int) -> Result<c_int, Error> {
    match rc {
        rc if rc >= CHU_OK => Ok(rc),
        CHU_EILLEGAL => Err(Error::Illegal),
        CHU_ESYNTAX => Err(Error::Syntax),
        rc => Err(Error::Engine(format!("libchushogi returned {rc}"))),
    }
}

fn color_code(c: Color) -> c_int {
    match c {
        Color::Sente => 0,
        Color::Gote => 1,
    }
}

impl FfiReferee {
    pub fn new() -> Result<Self, Error> {
        NonNull::new(unsafe { chu_new() })
            .map(|pos| FfiReferee { pos })
            .ok_or_else(|| Error::Engine("chu_new failed".into()))
    }

    /// Calls a string-producing function with a growing buffer.
    fn read_string(
        &self,
        mut cap: usize,
        f: unsafe extern "C" fn(*const ChuPos, *mut c_char, c_int) -> c_int,
    ) -> Result<(c_int, String), Error> {
        loop {
            let mut buf = vec![0u8; cap];
            let rc = unsafe { f(self.pos.as_ptr(), buf.as_mut_ptr().cast(), cap as c_int) };
            if rc == CHU_ESPACE {
                cap *= 2;
                continue;
            }
            let n = code(rc)?;
            let s = CStr::from_bytes_until_nul(&buf)
                .map_err(|e| Error::Engine(e.to_string()))?
                .to_str()
                .map_err(|e| Error::Engine(e.to_string()))?
                .to_owned();
            return Ok((n, s));
        }
    }
}

impl Referee for FfiReferee {
    fn set_sfen(&mut self, sfen: &str) -> Result<(), Error> {
        if !sfen_shape_ok(sfen) {
            return Err(Error::Syntax);
        }
        let c = CString::new(sfen).map_err(|_| Error::Syntax)?;
        code(unsafe { chu_set_sfen(self.pos.as_ptr(), c.as_ptr()) }).map(drop)
    }

    fn set_rules(&mut self, rules: Rules) -> Result<(), Error> {
        let r = match rules {
            Rules::Lishogi => 0,
            Rules::Jcsa => 1,
        };
        code(unsafe { chu_set_rules(self.pos.as_ptr(), r) }).map(drop)
    }

    fn apply(&mut self, usi: &str) -> Result<(), Error> {
        let c = CString::new(usi).map_err(|_| Error::Syntax)?;
        code(unsafe { chu_move(self.pos.as_ptr(), c.as_ptr()) }).map(drop)
    }

    fn legal(&mut self) -> Result<Vec<String>, Error> {
        let (_, s) = self.read_string(CHU_LEGAL_CAP, chu_legal)?;
        Ok(s.split_ascii_whitespace().map(str::to_owned).collect())
    }

    fn status(&mut self) -> Result<Status, Error> {
        let mut winner: c_int = -1;
        let end = match code(unsafe { chu_status(self.pos.as_ptr(), &mut winner) })? {
            0 => GameEnd::Playing,
            1 => GameEnd::RoyalsLost,
            2 => GameEnd::BareKing,
            3 => GameEnd::Stalemate,
            4 => GameEnd::Draw,
            n => return Err(Error::Engine(format!("chu_status returned {n}"))),
        };
        let winner = match winner {
            0 => Some(Color::Sente),
            1 => Some(Color::Gote),
            _ => None,
        };
        Ok(Status { end, winner })
    }

    fn in_check(&mut self, side: Color) -> Result<bool, Error> {
        code(unsafe { chu_in_check(self.pos.as_ptr(), color_code(side)) }).map(|v| v == 1)
    }

    fn sfen(&mut self) -> Result<String, Error> {
        self.read_string(CHU_SFEN_CAP, chu_sfen).map(|(_, s)| s)
    }

    fn ply(&mut self) -> Result<u32, Error> {
        code(unsafe { chu_ply(self.pos.as_ptr()) }).map(|n| n as u32)
    }
}
