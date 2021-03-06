//! Blake2B hash function

// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::iter::repeat;
use cryptoutil::{copy_memory, read_u64v_le, write_u64v_le};
use digest::Digest;
use mac::{Mac, MacResult};
use util::secure_memset;

static IV : [u64; 8] = [
  0x6a09e667f3bcc908, 0xbb67ae8584caa73b,
  0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
  0x510e527fade682d1, 0x9b05688c2b3e6c1f,
  0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

static SIGMA : [[usize; 16]; 12] = [
  [  0,  1,  2,  3,  4,  5,  6,  7,  8,  9, 10, 11, 12, 13, 14, 15 ],
  [ 14, 10,  4,  8,  9, 15, 13,  6,  1, 12,  0,  2, 11,  7,  5,  3 ],
  [ 11,  8, 12,  0,  5,  2, 15, 13, 10, 14,  3,  6,  7,  1,  9,  4 ],
  [  7,  9,  3,  1, 13, 12, 11, 14,  2,  6,  5, 10,  4,  0, 15,  8 ],
  [  9,  0,  5,  7,  2,  4, 10, 15, 14,  1, 11, 12,  6,  8,  3, 13 ],
  [  2, 12,  6, 10,  0, 11,  8,  3,  4, 13,  7,  5, 15, 14,  1,  9 ],
  [ 12,  5,  1, 15, 14, 13,  4, 10,  0,  7,  6,  3,  9,  2,  8, 11 ],
  [ 13, 11,  7, 14, 12,  1,  3,  9,  5,  0, 15,  4,  8,  6,  2, 10 ],
  [  6, 15, 14,  9, 11,  3,  0,  8, 12,  2, 13,  7,  1,  4, 10,  5 ],
  [ 10,  2,  8,  4,  7,  6,  1,  5, 15, 11,  9, 14,  3, 12, 13 , 0 ],
  [  0,  1,  2,  3,  4,  5,  6,  7,  8,  9, 10, 11, 12, 13, 14, 15 ],
  [ 14, 10,  4,  8,  9, 15, 13,  6,  1, 12,  0,  2, 11,  7,  5,  3 ],
];

const BLAKE2B_BLOCKBYTES : usize = 128;
const BLAKE2B_OUTBYTES : usize = 64;
const BLAKE2B_KEYBYTES : usize = 64;
const BLAKE2B_SALTBYTES : usize = 16;
const BLAKE2B_PERSONALBYTES : usize = 16;

/// Blake2b Context
#[derive(Copy)]
pub struct Blake2b {
    h: [u64; 8],
    t: [u64; 2],
    f: [u64; 2],
    buf: [u8; 2*BLAKE2B_BLOCKBYTES],
    buflen: usize,
    key: [u8; BLAKE2B_KEYBYTES],
    key_length: u8,
    last_node: u8,
    digest_length: u8,
    computed: bool, // whether the final digest has been computed
    param: Blake2bParam
}

impl Clone for Blake2b { fn clone(&self) -> Blake2b { *self } }

#[derive(Copy, Clone)]
struct Blake2bParam {
    digest_length: u8,
    key_length: u8,
    fanout: u8,
    depth: u8,
    leaf_length: u32,
    node_offset: u64,
    node_depth: u8,
    inner_length: u8,
    reserved: [u8; 14],
    salt: [u8; BLAKE2B_SALTBYTES],
    personal: [u8; BLAKE2B_PERSONALBYTES],
}

macro_rules! G( ($r:expr, $i:expr, $a:expr, $b:expr, $c:expr, $d:expr, $m:expr) => ({
    $a = $a.wrapping_add($b).wrapping_add($m[SIGMA[$r][2*$i+0]]);
    $d = ($d ^ $a).rotate_right(32);
    $c = $c.wrapping_add($d);
    $b = ($b ^ $c).rotate_right(24);
    $a = $a.wrapping_add($b).wrapping_add($m[SIGMA[$r][2*$i+1]]);
    $d = ($d ^ $a).rotate_right(16);
    $c = $c .wrapping_add($d);
    $b = ($b ^ $c).rotate_right(63);
}));

macro_rules! round( ($r:expr, $v:expr, $m:expr) => ( {
    G!($r,0,$v[ 0],$v[ 4],$v[ 8],$v[12], $m);
    G!($r,1,$v[ 1],$v[ 5],$v[ 9],$v[13], $m);
    G!($r,2,$v[ 2],$v[ 6],$v[10],$v[14], $m);
    G!($r,3,$v[ 3],$v[ 7],$v[11],$v[15], $m);
    G!($r,4,$v[ 0],$v[ 5],$v[10],$v[15], $m);
    G!($r,5,$v[ 1],$v[ 6],$v[11],$v[12], $m);
    G!($r,6,$v[ 2],$v[ 7],$v[ 8],$v[13], $m);
    G!($r,7,$v[ 3],$v[ 4],$v[ 9],$v[14], $m);
  }
));

impl Blake2b {
    fn set_lastnode(&mut self) {
        self.f[1] = 0xFFFFFFFFFFFFFFFF;
    }

    fn set_lastblock(&mut self) {
        if self.last_node!=0 {
            self.set_lastnode();
        }
        self.f[0] = 0xFFFFFFFFFFFFFFFF;
    }

    fn increment_counter(&mut self, inc : u64) {
        self.t[0] += inc;
        self.t[1] += if self.t[0] < inc { 1 } else { 0 };
    }

    fn init0(param: Blake2bParam, digest_length: u8, key: &[u8]) -> Blake2b {
        assert!(key.len() <= BLAKE2B_KEYBYTES);
        let mut b = Blake2b {
            h: IV,
            t: [0,0],
            f: [0,0],
            buf: [0; 2*BLAKE2B_BLOCKBYTES],
            buflen: 0,
            last_node: 0,
            digest_length: digest_length,
            computed: false,
            key: [0; BLAKE2B_KEYBYTES],
            key_length: key.len() as u8,
            param: param
        };
        copy_memory(key, &mut b.key);
        b
    }

    fn apply_param(&mut self) {
        use std::io::Write;
        use cryptoutil::WriteExt;

        let mut param_bytes : [u8; 64] = [0; 64];
        {
            let mut writer: &mut [u8] = &mut param_bytes;
            writer.write_u8(self.param.digest_length).unwrap();
            writer.write_u8(self.param.key_length).unwrap();
            writer.write_u8(self.param.fanout).unwrap();
            writer.write_u8(self.param.depth).unwrap();
            writer.write_u32_le(self.param.leaf_length).unwrap();
            writer.write_u64_le(self.param.node_offset).unwrap();
            writer.write_u8(self.param.node_depth).unwrap();
            writer.write_u8(self.param.inner_length).unwrap();
            writer.write_all(&self.param.reserved).unwrap();
            writer.write_all(&self.param.salt).unwrap();
            writer.write_all(&self.param.personal).unwrap();
        }

        let mut param_words : [u64; 8] = [0; 8];
        read_u64v_le(&mut param_words, &param_bytes);
        for (h, param_word) in self.h.iter_mut().zip(param_words.iter()) {
            *h = *h ^ *param_word;
        }
    }


    // init xors IV with input parameter block
    fn init_param( p: Blake2bParam, key: &[u8] ) -> Blake2b {
        let mut b = Blake2b::init0(p, p.digest_length, key);
        b.apply_param();
        b
    }

    fn default_param(outlen: u8) -> Blake2bParam {
        Blake2bParam {
            digest_length: outlen,
            key_length: 0,
            fanout: 1,
            depth: 1,
            leaf_length: 0,
            node_offset: 0,
            node_depth: 0,
            inner_length: 0,
            reserved: [0; 14],
            salt: [0; BLAKE2B_SALTBYTES],
            personal: [0; BLAKE2B_PERSONALBYTES],
        }
    }

    /// Create a new Blake2b context with a specific output size in bytes
    ///
    /// the size need to be between 0 (non included) and 64 bytes (included)
    pub fn new(outlen: usize) -> Blake2b {
        assert!(outlen > 0 && outlen <= BLAKE2B_OUTBYTES);
        Blake2b::init_param(Blake2b::default_param(outlen as u8), &[])
    }

    fn apply_key(&mut self) {
        let mut block : [u8; BLAKE2B_BLOCKBYTES] = [0; BLAKE2B_BLOCKBYTES];
        copy_memory(&self.key[..self.key_length as usize], &mut block);
        self.update(&block);
        secure_memset(&mut block[..], 0);
    }

    /// Similar to `new` but also takes a variable size key
    /// to tweak the context initialization
    pub fn new_keyed(outlen: usize, key: &[u8] ) -> Blake2b {
        assert!(outlen > 0 && outlen <= BLAKE2B_OUTBYTES);
        assert!(!key.is_empty() && key.len() <= BLAKE2B_KEYBYTES);

        let param = Blake2bParam {
            digest_length: outlen as u8,
            key_length: key.len() as u8,
            fanout: 1,
            depth: 1,
            leaf_length: 0,
            node_offset: 0,
            node_depth: 0,
            inner_length: 0,
            reserved: [0; 14],
            salt: [0; BLAKE2B_SALTBYTES],
            personal: [0; BLAKE2B_PERSONALBYTES],
        };

        let mut b = Blake2b::init_param(param, key);
        b.apply_key();
        b
    }

    fn compress(&mut self) {
        let mut ms: [u64; 16] = [0; 16];
        let mut vs: [u64; 16] = [0; 16];

        read_u64v_le(&mut ms, &self.buf[0..BLAKE2B_BLOCKBYTES]);

        for (v, h) in vs.iter_mut().zip(self.h.iter()) {
            *v = *h;
        }

        vs[ 8] = IV[0];
        vs[ 9] = IV[1];
        vs[10] = IV[2];
        vs[11] = IV[3];
        vs[12] = self.t[0] ^ IV[4];
        vs[13] = self.t[1] ^ IV[5];
        vs[14] = self.f[0] ^ IV[6];
        vs[15] = self.f[1] ^ IV[7];
        round!(  0, vs, ms );
        round!(  1, vs, ms );
        round!(  2, vs, ms );
        round!(  3, vs, ms );
        round!(  4, vs, ms );
        round!(  5, vs, ms );
        round!(  6, vs, ms );
        round!(  7, vs, ms );
        round!(  8, vs, ms );
        round!(  9, vs, ms );
        round!( 10, vs, ms );
        round!( 11, vs, ms );

        for (h_elem, (v_low, v_high)) in self.h.iter_mut().zip( vs[0..8].iter().zip(vs[8..16].iter()) ) {
            *h_elem = *h_elem ^ *v_low ^ *v_high;
        }
    }

    fn update( &mut self, mut input: &[u8] ) {
        while !input.is_empty() {
            let left = self.buflen;
            let fill = 2 * BLAKE2B_BLOCKBYTES - left;

            if input.len() > fill {
                copy_memory(&input[0..fill], &mut self.buf[left..]); // Fill buffer
                self.buflen += fill;
                self.increment_counter( BLAKE2B_BLOCKBYTES as u64);
                self.compress();

                let mut halves = self.buf.chunks_mut(BLAKE2B_BLOCKBYTES);
                let first_half = halves.next().unwrap();
                let second_half = halves.next().unwrap();
                copy_memory(second_half, first_half);

                self.buflen -= BLAKE2B_BLOCKBYTES;
                input = &input[fill..input.len()];
            } else { // inlen <= fill
                copy_memory(input, &mut self.buf[left..]);
                self.buflen += input.len();
                break;
            }
        }
    }

    fn finalize( &mut self, out: &mut [u8] ) {
        assert!(out.len() == self.digest_length as usize);
        if !self.computed {
            if self.buflen > BLAKE2B_BLOCKBYTES {
                self.increment_counter(BLAKE2B_BLOCKBYTES as u64);
                self.compress();
                self.buflen -= BLAKE2B_BLOCKBYTES;

                let mut halves = self.buf.chunks_mut(BLAKE2B_BLOCKBYTES);
                let first_half = halves.next().unwrap();
                let second_half = halves.next().unwrap();
                copy_memory(second_half, first_half);
            }

            let incby = self.buflen as u64;
            self.increment_counter(incby);
            self.set_lastblock();
            for b in self.buf[self.buflen..].iter_mut() {
                *b = 0;
            }
            self.compress();

            write_u64v_le(&mut self.buf[0..64], &self.h);
            self.computed = true;
        }
        let outlen = out.len();
        copy_memory(&self.buf[0..outlen], out);
    }

    /// Reset the context to the state after calling `new`
    pub fn reset(&mut self) {
        for (h_elem, iv_elem) in self.h.iter_mut().zip(IV.iter()) {
            *h_elem = *iv_elem;
        }
        for t_elem in self.t.iter_mut() {
            *t_elem = 0;
        }
        for f_elem in self.f.iter_mut() {
            *f_elem = 0;
        }
        for b in self.buf.iter_mut() {
            *b = 0;
        }
        self.buflen = 0;
        self.last_node = 0;
        self.computed = false;
        self.apply_param();
        if self.key_length > 0 {
            self.apply_key();
        }
    }

    pub fn blake2b(out: &mut[u8], input: &[u8], key: &[u8]) {
        let mut hasher : Blake2b = if !key.is_empty() { Blake2b::new_keyed(out.len(), key) } else { Blake2b::new(out.len()) };

        hasher.update(input);
        hasher.finalize(out);
    }
}

impl Digest for Blake2b {
    fn reset(&mut self) { Blake2b::reset(self); }
    fn input(&mut self, msg: &[u8]) { self.update(msg); }
    fn result(&mut self, out: &mut [u8]) { self.finalize(out); }
    fn output_bits(&self) -> usize { 8 * (self.digest_length as usize) }
    fn block_size(&self) -> usize { 8 * BLAKE2B_BLOCKBYTES }
}

impl Mac for Blake2b {
    /**
     * Process input data.
     *
     * # Arguments
     * * data - The input data to process.
     *
     */
    fn input(&mut self, data: &[u8]) {
        self.update(data);
    }

    /**
     * Reset the Mac state to begin processing another input stream.
     */
    fn reset(&mut self) {
        Blake2b::reset(self);
    }

    /**
     * Obtain the result of a Mac computation as a MacResult.
     */
    fn result(&mut self) -> MacResult {
        let mut mac: Vec<u8> = repeat(0).take(self.digest_length as usize).collect();
        self.raw_result(&mut mac);
        MacResult::new_from_owned(mac)
    }

    /**
     * Obtain the result of a Mac computation as [u8]. This method should be used very carefully
     * since incorrect use of the Mac code could result in permitting a timing attack which defeats
     * the security provided by a Mac function.
     */
    fn raw_result(&mut self, output: &mut [u8]) {
        self.finalize(output);
    }

    /**
     * Get the size of the Mac code, in bytes.
     */
    fn output_bytes(&self) -> usize { self.digest_length as usize }
}


#[cfg(test)]
mod mac_tests {
    use blake2b::Blake2b;
    use mac::Mac;

    #[test]
    fn test_blake2b_mac() {
        let key: Vec<u8> = (0..64).map(|i| i).collect();
        let mut m = Blake2b::new_keyed(64, &key[..]);
        m.input(&[1,2,4,8]);
        let expected = [
            0x8e, 0xc6, 0xcb, 0x71, 0xc4, 0x5c, 0x3c, 0x90,
            0x91, 0xd0, 0x8a, 0x37, 0x1e, 0xa8, 0x5d, 0xc1,
            0x22, 0xb5, 0xc8, 0xe2, 0xd9, 0xe5, 0x71, 0x42,
            0xbf, 0xef, 0xce, 0x42, 0xd7, 0xbc, 0xf8, 0x8b,
            0xb0, 0x31, 0x27, 0x88, 0x2e, 0x51, 0xa9, 0x21,
            0x44, 0x62, 0x08, 0xf6, 0xa3, 0x58, 0xa9, 0xe0,
            0x7d, 0x35, 0x3b, 0xd3, 0x1c, 0x41, 0x70, 0x15,
            0x62, 0xac, 0xd5, 0x39, 0x4e, 0xee, 0x73, 0xae,
        ];
        assert_eq!(m.result().code().to_vec(), expected.to_vec());
    }
}

#[cfg(all(test, feature = "with-bench"))]
mod bench {
    use test::Bencher;

    use digest::Digest;
    use blake2b::Blake2b;


    #[bench]
    pub fn blake2b_10(bh: & mut Bencher) {
        let mut sh = Blake2b::new(64);
        let bytes = [1u8; 10];
        bh.iter( || {
            sh.input(&bytes);
        });
        bh.bytes = bytes.len() as u64;
    }

    #[bench]
    pub fn blake2b_1k(bh: & mut Bencher) {
        let mut sh = Blake2b::new(64);
        let bytes = [1u8; 1024];
        bh.iter( || {
            sh.input(&bytes);
        });
        bh.bytes = bytes.len() as u64;
    }

    #[bench]
    pub fn blake2b_64k(bh: & mut Bencher) {
        let mut sh = Blake2b::new(64);
        let bytes = [1u8; 65536];
        bh.iter( || {
            sh.input(&bytes);
        });
        bh.bytes = bytes.len() as u64;
    }
}
