// Copyright (c) Radzivon Bartoshyk. All rights reserved.
//
// Redistribution and use in source and binary forms, with or without modification,
// are permitted provided that the following conditions are met:
//
// 1.  Redistributions of source code must retain the above copyright notice, this
// list of conditions and the following disclaimer.
//
// 2.  Redistributions in binary form must reproduce the above copyright notice,
// this list of conditions and the following disclaimer in the documentation
// and/or other materials provided with the distribution.
//
// 3.  Neither the name of the copyright holder nor the names of its
// contributors may be used to endorse or promote products derived from
// this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::reflect_101;
use crate::reflect_index;
use crate::sse::{_mm_opt_fmlaf_ps, _mm_opt_fnmlaf_ps, _mm_opt_fnmlsf_ps, load_f32, store_f32};
use crate::unsafe_slice::UnsafeSlice;
use crate::{clamp_edge, EdgeMode};

pub(crate) fn fgn_vertical_pass_sse_f32<T, const CHANNELS_COUNT: usize>(
    undefined_slice: &UnsafeSlice<T>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    unsafe {
        let bytes: &UnsafeSlice<'_, f32> = std::mem::transmute(undefined_slice);
        if std::arch::is_x86_feature_detected!("fma") {
            fgn_vertical_pass_sse_f32_fma::<CHANNELS_COUNT>(
                bytes, stride, width, height, radius, start, end, edge_mode,
            );
        } else {
            fgn_vertical_pass_sse_f32_def::<CHANNELS_COUNT>(
                bytes, stride, width, height, radius, start, end, edge_mode,
            );
        }
    }
}

#[target_feature(enable = "sse4.1")]
unsafe fn fgn_vertical_pass_sse_f32_def<const CHANNELS_COUNT: usize>(
    bytes: &UnsafeSlice<f32>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    fgn_vertical_pass_sse_f32_impl::<CHANNELS_COUNT, false>(
        bytes, stride, width, height, radius, start, end, edge_mode,
    );
}

#[target_feature(enable = "sse4.1", enable = "fma")]
unsafe fn fgn_vertical_pass_sse_f32_fma<const CHANNELS_COUNT: usize>(
    bytes: &UnsafeSlice<f32>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    fgn_vertical_pass_sse_f32_impl::<CHANNELS_COUNT, true>(
        bytes, stride, width, height, radius, start, end, edge_mode,
    );
}

#[inline(always)]
unsafe fn fgn_vertical_pass_sse_f32_impl<const CHANNELS_COUNT: usize, const FMA: bool>(
    bytes: &UnsafeSlice<f32>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    let mut buffer = Box::new([[0.; 4]; 1024]);

    let height_wide = height as i64;

    let threes = _mm_set1_ps(3.);

    let radius_64 = radius as i64;
    let weight = 1.0f32 / ((radius as f32) * (radius as f32) * (radius as f32));
    let v_weight = _mm_set1_ps(weight);
    for x in start..width.min(end) {
        let mut diffs = _mm_setzero_ps();
        let mut ders = _mm_setzero_ps();
        let mut summs = _mm_setzero_ps();

        let current_px = (x * CHANNELS_COUNT as u32) as usize;

        let start_y = 0 - 3 * radius as i64;
        for y in start_y..height_wide {
            if y >= 0 {
                let current_y = (y * (stride as i64)) as usize;
                let bytes_offset = current_y + current_px;

                let pixel = _mm_mul_ps(summs, v_weight);
                let dst_ptr = bytes.slice.as_ptr().add(bytes_offset) as *mut f32;
                store_f32::<CHANNELS_COUNT>(dst_ptr, pixel);

                let d_arr_index_1 = ((y + radius_64) & 1023) as usize;
                let d_arr_index_2 = ((y - radius_64) & 1023) as usize;
                let d_arr_index = (y & 1023) as usize;

                let buf_ptr = buffer.get_unchecked_mut(d_arr_index).as_mut_ptr();
                let stored = _mm_loadu_ps(buf_ptr);

                let buf_ptr_1 = buffer.as_mut_ptr().add(d_arr_index_1) as *mut f32;
                let stored_1 = _mm_loadu_ps(buf_ptr_1);

                let buf_ptr_2 = buffer.as_mut_ptr().add(d_arr_index_2) as *mut f32;
                let stored_2 = _mm_loadu_ps(buf_ptr_2);

                let new_diff =
                    _mm_opt_fnmlsf_ps::<FMA>(stored_2, _mm_sub_ps(stored, stored_1), threes);
                diffs = _mm_add_ps(diffs, new_diff);
            } else if y + radius_64 >= 0 {
                let arr_index = (y & 1023) as usize;
                let arr_index_1 = ((y + radius_64) & 1023) as usize;
                let buf_ptr = buffer.get_unchecked_mut(arr_index).as_mut_ptr();
                let stored = _mm_loadu_ps(buf_ptr);

                let buf_ptr_1 = buffer.get_unchecked_mut(arr_index_1).as_mut_ptr();
                let stored_1 = _mm_loadu_ps(buf_ptr_1);

                diffs = _mm_opt_fmlaf_ps::<FMA>(diffs, _mm_sub_ps(stored, stored_1), threes);
            } else if y + 2 * radius_64 >= 0 {
                let arr_index = ((y + radius_64) & 1023) as usize;
                let buf_ptr = buffer.get_unchecked_mut(arr_index).as_mut_ptr();
                let stored = _mm_loadu_ps(buf_ptr);
                diffs = _mm_opt_fnmlaf_ps::<FMA>(diffs, stored, threes);
            }

            let next_row_y = clamp_edge!(edge_mode, y + ((3 * radius_64) >> 1), 0, height_wide - 1)
                * (stride as usize);
            let next_row_x = (x * CHANNELS_COUNT as u32) as usize;

            let s_ptr = bytes.slice.as_ptr().add(next_row_y + next_row_x) as *mut f32;

            let pixel_color = load_f32::<CHANNELS_COUNT>(s_ptr);

            let arr_index = ((y + 2 * radius_64) & 1023) as usize;
            let buf_ptr = buffer.get_unchecked_mut(arr_index).as_mut_ptr();

            diffs = _mm_add_ps(diffs, pixel_color);
            ders = _mm_add_ps(ders, diffs);
            summs = _mm_add_ps(summs, ders);
            _mm_storeu_ps(buf_ptr, pixel_color);
        }
    }
}

pub(crate) fn fgn_horizontal_pass_sse_f32<T, const CHANNELS_COUNT: usize>(
    undefined_slice: &UnsafeSlice<T>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    unsafe {
        let bytes: &UnsafeSlice<'_, f32> = std::mem::transmute(undefined_slice);
        if std::arch::is_x86_feature_detected!("fma") {
            fgn_horizontal_pass_sse_f32_fma::<CHANNELS_COUNT>(
                bytes, stride, width, height, radius, start, end, edge_mode,
            );
        } else {
            fgn_horizontal_pass_sse_f32_def::<CHANNELS_COUNT>(
                bytes, stride, width, height, radius, start, end, edge_mode,
            );
        }
    }
}

#[target_feature(enable = "sse4.1")]
unsafe fn fgn_horizontal_pass_sse_f32_def<const CHANNELS_COUNT: usize>(
    bytes: &UnsafeSlice<f32>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    fgn_horizontal_pass_sse_f32_impl::<CHANNELS_COUNT, false>(
        bytes, stride, width, height, radius, start, end, edge_mode,
    );
}

#[target_feature(enable = "sse4.1", enable = "fma")]
unsafe fn fgn_horizontal_pass_sse_f32_fma<const CHANNELS_COUNT: usize>(
    bytes: &UnsafeSlice<f32>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    fgn_horizontal_pass_sse_f32_impl::<CHANNELS_COUNT, true>(
        bytes, stride, width, height, radius, start, end, edge_mode,
    );
}

#[inline(always)]
unsafe fn fgn_horizontal_pass_sse_f32_impl<const CN: usize, const FMA: bool>(
    bytes: &UnsafeSlice<f32>,
    stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    start: u32,
    end: u32,
    edge_mode: EdgeMode,
) {
    let mut buffer = Box::new([[0.; 4]; 1024]);

    let width_wide = width as i64;

    let threes = _mm_set1_ps(3.);

    let radius_64 = radius as i64;
    let weight = 1.0f32 / ((radius as f32) * (radius as f32) * (radius as f32));
    let v_weight = _mm_set1_ps(weight);
    for y in start..height.min(end) {
        let mut diffs = _mm_setzero_ps();
        let mut ders = _mm_setzero_ps();
        let mut summs = _mm_setzero_ps();

        let current_y = ((y as i64) * (stride as i64)) as usize;

        for x in (0 - 3 * radius_64)..(width as i64) {
            if x >= 0 {
                let current_px = x as usize * CN;

                let bytes_offset = current_y + current_px;

                let pixel = _mm_mul_ps(summs, v_weight);
                let dst_ptr = bytes.slice.as_ptr().add(bytes_offset) as *mut f32;
                store_f32::<CN>(dst_ptr, pixel);

                let d_arr_index_1 = ((x + radius_64) & 1023) as usize;
                let d_arr_index_2 = ((x - radius_64) & 1023) as usize;
                let d_arr_index = (x & 1023) as usize;

                let buf_ptr = buffer.get_unchecked_mut(d_arr_index).as_mut_ptr();
                let stored = _mm_loadu_ps(buf_ptr);

                let buf_ptr_1 = buffer.get_unchecked_mut(d_arr_index_1).as_mut_ptr();
                let stored_1 = _mm_loadu_ps(buf_ptr_1);

                let buf_ptr_2 = buffer.get_unchecked_mut(d_arr_index_2).as_mut_ptr();
                let stored_2 = _mm_loadu_ps(buf_ptr_2);

                let new_diff =
                    _mm_opt_fnmlsf_ps::<FMA>(stored_2, _mm_sub_ps(stored, stored_1), threes);
                diffs = _mm_add_ps(diffs, new_diff);
            } else if x + radius_64 >= 0 {
                let arr_index = (x & 1023) as usize;
                let arr_index_1 = ((x + radius_64) & 1023) as usize;
                let buf_ptr = buffer.as_mut_ptr().add(arr_index) as *mut f32;
                let stored = _mm_loadu_ps(buf_ptr);

                let buf_ptr_1 = buffer.as_mut_ptr().add(arr_index_1);
                let stored_1 = _mm_loadu_ps(buf_ptr_1 as *const f32);

                diffs = _mm_opt_fmlaf_ps::<FMA>(diffs, _mm_sub_ps(stored, stored_1), threes);
            } else if x + 2 * radius_64 >= 0 {
                let arr_index = ((x + radius_64) & 1023) as usize;
                let buf_ptr = buffer.as_mut_ptr().add(arr_index);
                let stored = _mm_loadu_ps(buf_ptr as *const f32);
                diffs = _mm_opt_fnmlaf_ps::<FMA>(diffs, stored, threes);
            }

            let next_row_y = (y as usize) * (stride as usize);
            let next_row_x = clamp_edge!(edge_mode, x + 3 * radius_64 / 2, 0, width_wide - 1);
            let next_row_px = next_row_x * CN;

            let s_ptr = bytes.slice.as_ptr().add(next_row_y + next_row_px) as *mut f32;

            let pixel_color = load_f32::<CN>(s_ptr);

            let arr_index = ((x + 2 * radius_64) & 1023) as usize;
            let buf_ptr = buffer.get_unchecked_mut(arr_index).as_mut_ptr();

            diffs = _mm_add_ps(diffs, pixel_color);
            ders = _mm_add_ps(ders, diffs);
            summs = _mm_add_ps(summs, ders);
            _mm_storeu_ps(buf_ptr, pixel_color);
        }
    }
}
