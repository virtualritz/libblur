/*
 * // Copyright (c) Radzivon Bartoshyk. All rights reserved.
 * //
 * // Redistribution and use in source and binary forms, with or without modification,
 * // are permitted provided that the following conditions are met:
 * //
 * // 1.  Redistributions of source code must retain the above copyright notice, this
 * // list of conditions and the following disclaimer.
 * //
 * // 2.  Redistributions in binary form must reproduce the above copyright notice,
 * // this list of conditions and the following disclaimer in the documentation
 * // and/or other materials provided with the distribution.
 * //
 * // 3.  Neither the name of the copyright holder nor the names of its
 * // contributors may be used to endorse or promote products derived from
 * // this software without specific prior written permission.
 * //
 * // THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
 * // AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 * // IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * // DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
 * // FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * // DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * // SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * // CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * // OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * // OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */
use crate::filter1d::arena::Arena;
use crate::filter1d::filter_scan::ScanPoint1d;
use crate::filter1d::region::FilterRegion;
use crate::img_size::ImageSize;
use crate::neon::{prefer_vfma_f32, prefer_vfmaq_f32};
use crate::to_storage::ToStorage;
use crate::unsafe_slice::UnsafeSlice;
use num_traits::MulAdd;
use std::arch::aarch64::*;
use std::ops::{Add, Mul};

pub fn filter_column_neon_symm_f32_f32(
    arena: Arena,
    arena_src: &[&[f32]],
    dst: &UnsafeSlice<f32>,
    image_size: ImageSize,
    filter_region: FilterRegion,
    scanned_kernel: &[ScanPoint1d<f32>],
) {
    unsafe {
        let dst_stride = image_size.width * arena.components;

        let y = filter_region.start;

        let length = scanned_kernel.len();
        let half_len = length / 2;

        let mut _cx = 0usize;

        while _cx + 16 < dst_stride {
            let coeff = vdupq_n_f32(scanned_kernel.get_unchecked(half_len).weight);

            let v_src = arena_src.get_unchecked(half_len).get_unchecked(_cx..);

            let source = vld1q_f32_x4(v_src.as_ptr());
            let mut k0 = vmulq_f32(source.0, coeff);
            let mut k1 = vmulq_f32(source.1, coeff);
            let mut k2 = vmulq_f32(source.2, coeff);
            let mut k3 = vmulq_f32(source.3, coeff);

            for i in 0..half_len {
                let rollback = length - i - 1;
                let coeff = vdupq_n_f32(scanned_kernel.get_unchecked(i).weight);
                let v_source0 =
                    vld1q_f32_x4(arena_src.get_unchecked(i).get_unchecked(_cx..).as_ptr());
                let v_source1 = vld1q_f32_x4(
                    arena_src
                        .get_unchecked(rollback)
                        .get_unchecked(_cx..)
                        .as_ptr(),
                );
                k0 = prefer_vfmaq_f32(k0, vaddq_f32(v_source0.0, v_source1.0), coeff);
                k1 = prefer_vfmaq_f32(k1, vaddq_f32(v_source0.1, v_source1.1), coeff);
                k2 = prefer_vfmaq_f32(k2, vaddq_f32(v_source0.2, v_source1.2), coeff);
                k3 = prefer_vfmaq_f32(k3, vaddq_f32(v_source0.3, v_source1.3), coeff);
            }

            let dst_offset = y * dst_stride + _cx;
            let dst_ptr0 = (dst.slice.as_ptr() as *mut f32).add(dst_offset);
            vst1q_f32_x4(dst_ptr0, float32x4x4_t(k0, k1, k2, k3));
            _cx += 16;
        }

        while _cx + 8 < dst_stride {
            let coeff = vdupq_n_f32(scanned_kernel.get_unchecked(half_len).weight);

            let v_src = arena_src.get_unchecked(half_len).get_unchecked(_cx..);

            let source = vld1q_f32_x2(v_src.as_ptr());
            let mut k0 = vmulq_f32(source.0, coeff);
            let mut k1 = vmulq_f32(source.1, coeff);

            for i in 0..half_len {
                let rollback = length - i - 1;
                let coeff = vdupq_n_f32(scanned_kernel.get_unchecked(i).weight);
                let v_source0 =
                    vld1q_f32_x2(arena_src.get_unchecked(i).get_unchecked(_cx..).as_ptr());
                let v_source1 = vld1q_f32_x2(
                    arena_src
                        .get_unchecked(rollback)
                        .get_unchecked(_cx..)
                        .as_ptr(),
                );
                k0 = prefer_vfmaq_f32(k0, vaddq_f32(v_source0.0, v_source1.0), coeff);
                k1 = prefer_vfmaq_f32(k1, vaddq_f32(v_source0.1, v_source1.1), coeff);
            }

            let dst_offset = y * dst_stride + _cx;
            let dst_ptr0 = (dst.slice.as_ptr() as *mut f32).add(dst_offset);
            vst1q_f32_x2(dst_ptr0, float32x4x2_t(k0, k1));

            _cx += 8;
        }

        while _cx + 4 < dst_stride {
            let coeff = *scanned_kernel.get_unchecked(half_len);

            let v_src = arena_src.get_unchecked(half_len).get_unchecked(_cx..);

            let source_0 = vld1q_f32(v_src.as_ptr());
            let mut k0 = vmulq_f32(source_0, vdupq_n_f32(coeff.weight));

            for i in 0..half_len {
                let rollback = length - i - 1;
                let coeff = *scanned_kernel.get_unchecked(i);
                let v_source_0 =
                    vld1q_f32(arena_src.get_unchecked(i).get_unchecked(_cx..).as_ptr());
                let v_source_1 = vld1q_f32(
                    arena_src
                        .get_unchecked(rollback)
                        .get_unchecked(_cx..)
                        .as_ptr(),
                );
                k0 = prefer_vfmaq_f32(
                    k0,
                    vaddq_f32(v_source_0, v_source_1),
                    vdupq_n_f32(coeff.weight),
                );
            }

            let dst_offset = y * dst_stride + _cx;
            let dst_ptr = (dst.slice.as_ptr() as *mut f32).add(dst_offset);
            vst1q_f32(dst_ptr, k0);
            _cx += 4;
        }

        while _cx + 2 < dst_stride {
            let coeff = *scanned_kernel.get_unchecked(half_len);

            let v_src = arena_src.get_unchecked(half_len).get_unchecked(_cx..);

            let source_0 = vld1_f32(v_src.as_ptr());
            let mut k0 = vmul_f32(source_0, vdup_n_f32(coeff.weight));

            for i in 0..half_len {
                let rollback = length - i - 1;
                let coeff = *scanned_kernel.get_unchecked(i);
                let v_source_0 = vld1_f32(arena_src.get_unchecked(i).get_unchecked(_cx..).as_ptr());
                let v_source_1 = vld1_f32(
                    arena_src
                        .get_unchecked(rollback)
                        .get_unchecked(_cx..)
                        .as_ptr(),
                );
                k0 = prefer_vfma_f32(
                    k0,
                    vadd_f32(v_source_0, v_source_1),
                    vdup_n_f32(coeff.weight),
                );
            }

            let dst_offset = y * dst_stride + _cx;
            let dst_ptr = (dst.slice.as_ptr() as *mut f32).add(dst_offset);
            vst1_f32(dst_ptr, k0);
            _cx += 2;
        }

        for x in _cx..dst_stride {
            let coeff = *scanned_kernel.get_unchecked(half_len);

            let v_src = arena_src.get_unchecked(half_len).get_unchecked(_cx..);

            let mut k0 = (*v_src.get_unchecked(0)).mul(coeff.weight);

            for i in 0..half_len {
                let rollback = length - i - 1;
                let coeff = *scanned_kernel.get_unchecked(i);
                k0 = MulAdd::mul_add(
                    arena_src
                        .get_unchecked(i)
                        .get_unchecked(x)
                        .add(arena_src.get_unchecked(rollback).get_unchecked(x)),
                    coeff.weight,
                    k0,
                );
            }

            dst.write(y * dst_stride + x, k0.to_());
        }
    }
}