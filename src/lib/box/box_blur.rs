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

use colorutils_rs::{
    linear_to_rgb, linear_to_rgba, rgb_to_linear, rgba_to_linear, TransferFunction,
};
use num_traits::cast::FromPrimitive;
use num_traits::AsPrimitive;
use rayon::ThreadPool;

use crate::channels_configuration::FastBlurChannels;
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
use crate::r#box::box_blur_neon::neon_support;
#[cfg(all(
    any(target_arch = "x86_64", target_arch = "x86"),
    target_feature = "sse4.1"
))]
use crate::r#box::box_blur_sse::sse_support;
use crate::unsafe_slice::UnsafeSlice;
use crate::ThreadingPolicy;

fn box_blur_horizontal_pass_impl<
    T,
    J,
    const CHANNELS_CONFIGURATION: usize,
    const USE_ROUNDING: bool,
>(
    src: &[T],
    src_stride: u32,
    unsafe_dst: &UnsafeSlice<T>,
    dst_stride: u32,
    width: u32,
    radius: u32,
    start_y: u32,
    end_y: u32,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + FromPrimitive
        + Default
        + Send
        + Sync
        + AsPrimitive<J>,
    J: FromPrimitive
        + Copy
        + std::ops::Mul<Output = J>
        + std::ops::AddAssign
        + std::ops::SubAssign
        + AsPrimitive<f32>,
{
    let box_channels: FastBlurChannels = CHANNELS_CONFIGURATION.into();
    let kernel_size = radius * 2 + 1;
    let edge_count = J::from_u32((kernel_size / 2) + 1).unwrap();
    let half_kernel = kernel_size / 2;
    let channels_count = match box_channels {
        FastBlurChannels::Channels3 => 3,
        FastBlurChannels::Channels4 => 4,
    } as usize;

    let weight = 1f32 / (radius * 2) as f32;

    for y in start_y..end_y {
        let mut kernel: [J; 4] = [J::from_u32(0u32).unwrap(); 4];
        let y_src_shift = (y * src_stride) as usize;
        let y_dst_shift = (y * dst_stride) as usize;
        // replicate edge
        kernel[0] = (unsafe { *src.get_unchecked(y_src_shift) }.as_()) * edge_count;
        kernel[1] = (unsafe { *src.get_unchecked(y_src_shift + 1) }.as_()) * edge_count;
        kernel[2] = (unsafe { *src.get_unchecked(y_src_shift + 2) }.as_()) * edge_count;
        match box_channels {
            FastBlurChannels::Channels3 => {}
            FastBlurChannels::Channels4 => {
                kernel[3] = (unsafe { *src.get_unchecked(y_src_shift + 3) }.as_()) * edge_count;
            }
        }

        for x in 1..std::cmp::min(half_kernel, width) {
            let px = x as usize * channels_count;
            kernel[0] += unsafe { *src.get_unchecked(y_src_shift + px) }.as_();
            kernel[1] += unsafe { *src.get_unchecked(y_src_shift + px + 1) }.as_();
            kernel[2] += unsafe { *src.get_unchecked(y_src_shift + px + 2) }.as_();
            match box_channels {
                FastBlurChannels::Channels3 => {}
                FastBlurChannels::Channels4 => {
                    kernel[3] += unsafe { *src.get_unchecked(y_src_shift + px + 3) }.as_();
                }
            }
        }

        for x in 0..width {
            let next = std::cmp::min(x + half_kernel, width - 1) as usize * channels_count;
            let previous =
                std::cmp::max(x as i64 - half_kernel as i64, 0) as usize * channels_count;
            let px = x as usize * channels_count;
            // Prune previous and add next and compute mean

            kernel[0] += unsafe { *src.get_unchecked(y_src_shift + next) }.as_();
            kernel[1] += unsafe { *src.get_unchecked(y_src_shift + next + 1) }.as_();
            kernel[2] += unsafe { *src.get_unchecked(y_src_shift + next + 2) }.as_();

            kernel[0] -= unsafe { *src.get_unchecked(y_src_shift + previous) }.as_();
            kernel[1] -= unsafe { *src.get_unchecked(y_src_shift + previous + 1) }.as_();
            kernel[2] -= unsafe { *src.get_unchecked(y_src_shift + previous + 2) }.as_();

            match box_channels {
                FastBlurChannels::Channels3 => {}
                FastBlurChannels::Channels4 => {
                    kernel[3] += unsafe { *src.get_unchecked(y_src_shift + next + 3) }.as_();
                    kernel[3] -= unsafe { *src.get_unchecked(y_src_shift + previous + 3) }.as_();
                }
            }

            let write_offset = y_dst_shift + px;
            unsafe {
                if USE_ROUNDING {
                    unsafe_dst.write(
                        write_offset + 0,
                        T::from_f32((kernel[0].as_() * weight).round()).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 1,
                        T::from_f32((kernel[1].as_() * weight).round()).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 2,
                        T::from_f32((kernel[2].as_() * weight).round()).unwrap_or_default(),
                    );

                    match box_channels {
                        FastBlurChannels::Channels3 => {}
                        FastBlurChannels::Channels4 => {
                            unsafe_dst.write(
                                write_offset + 3,
                                T::from_f32((kernel[3].as_() * weight).round()).unwrap_or_default(),
                            );
                        }
                    }
                } else {
                    unsafe_dst.write(
                        write_offset + 0,
                        T::from_f32(kernel[0].as_() * weight).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 1,
                        T::from_f32(kernel[1].as_() * weight).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 2,
                        T::from_f32(kernel[2].as_() * weight).unwrap_or_default(),
                    );

                    match box_channels {
                        FastBlurChannels::Channels3 => {}
                        FastBlurChannels::Channels4 => {
                            unsafe_dst.write(
                                write_offset + 3,
                                T::from_f32(kernel[3].as_() * weight).unwrap_or_default(),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn box_blur_horizontal_pass<
    T: FromPrimitive + Default + Send + Sync,
    const CHANNEL_CONFIGURATION: usize,
>(
    src: &[T],
    src_stride: u32,
    dst: &mut [T],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    pool: &ThreadPool,
    thread_count: u32,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + AsPrimitive<u32>
        + AsPrimitive<u64>
        + AsPrimitive<f32>
        + AsPrimitive<f64>,
{
    let mut _dispatcher_horizontal: fn(
        src: &[T],
        src_stride: u32,
        unsafe_dst: &UnsafeSlice<T>,
        dst_stride: u32,
        width: u32,
        radius: u32,
        start_y: u32,
        end_y: u32,
    ) = box_blur_horizontal_pass_impl::<T, u32, CHANNEL_CONFIGURATION, false>;
    if std::any::type_name::<T>() == "u8" || std::any::type_name::<T>() == "u16" {
        _dispatcher_horizontal =
            box_blur_horizontal_pass_impl::<T, u32, CHANNEL_CONFIGURATION, true>;
    } else if std::any::type_name::<T>() == "f32" {
        _dispatcher_horizontal =
            box_blur_horizontal_pass_impl::<T, f32, CHANNEL_CONFIGURATION, false>;
    }
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        if std::any::type_name::<T>() == "u8" {
            _dispatcher_horizontal =
                neon_support::box_blur_horizontal_pass_neon::<T, CHANNEL_CONFIGURATION>;
        }
    }
    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "x86"),
        target_feature = "sse4.1"
    ))]
    {
        if std::any::type_name::<T>() == "u8" {
            _dispatcher_horizontal =
                sse_support::box_blur_horizontal_pass_sse::<T, { CHANNEL_CONFIGURATION }>;
        }
    }
    let unsafe_dst = UnsafeSlice::new(dst);
    pool.scope(|scope| {
        let segment_size = height / thread_count;
        for i in 0..thread_count {
            let start_y = i * segment_size;
            let mut end_y = (i + 1) * segment_size;
            if i == thread_count - 1 {
                end_y = height;
            }

            scope.spawn(move |_| {
                _dispatcher_horizontal(
                    src,
                    src_stride,
                    &unsafe_dst,
                    dst_stride,
                    width,
                    radius,
                    start_y,
                    end_y,
                );
            });
        }
    });
}

fn box_blur_vertical_pass_impl<T, J, const CHANNEL_CONFIGURATION: usize, const USE_ROUNDING: bool>(
    src: &[T],
    src_stride: u32,
    unsafe_dst: &UnsafeSlice<T>,
    dst_stride: u32,
    _: u32,
    height: u32,
    radius: u32,
    start_x: u32,
    end_x: u32,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + FromPrimitive
        + Default
        + Send
        + Sync
        + AsPrimitive<J>,
    J: FromPrimitive
        + Copy
        + std::ops::Mul<Output = J>
        + std::ops::AddAssign
        + std::ops::SubAssign
        + AsPrimitive<f32>,
{
    let box_channels: FastBlurChannels = CHANNEL_CONFIGURATION.into();
    let kernel_size = radius * 2 + 1;

    let edge_count = J::from_u32((kernel_size / 2) + 1).unwrap();
    let half_kernel = kernel_size / 2;
    let channels_count = match box_channels {
        FastBlurChannels::Channels3 => 3,
        FastBlurChannels::Channels4 => 4,
    };

    let weight = 1f32 / (radius * 2) as f32;

    for x in start_x..end_x {
        let mut kernel: [J; 4] = [J::from_u32(0u32).unwrap(); 4];
        // replicate edge
        let px = x as usize * channels_count;
        kernel[0] = (unsafe { *src.get_unchecked(px) }.as_()) * edge_count;
        kernel[1] = (unsafe { *src.get_unchecked(px + 1) }.as_()) * edge_count;
        kernel[2] = (unsafe { *src.get_unchecked(px + 2) }.as_()) * edge_count;
        match box_channels {
            FastBlurChannels::Channels3 => {}
            FastBlurChannels::Channels4 => {
                kernel[3] = (unsafe { *src.get_unchecked(px + 3) }.as_()) * edge_count;
            }
        }

        for y in 1..std::cmp::min(half_kernel, height) {
            let y_src_shift = y as usize * src_stride as usize;
            kernel[0] += unsafe { *src.get_unchecked(y_src_shift + px) }.as_();
            kernel[1] += unsafe { *src.get_unchecked(y_src_shift + px + 1) }.as_();
            kernel[2] += unsafe { *src.get_unchecked(y_src_shift + px + 2) }.as_();
            match box_channels {
                FastBlurChannels::Channels3 => {}
                FastBlurChannels::Channels4 => {
                    kernel[3] += unsafe { *src.get_unchecked(y_src_shift + px + 3) }.as_();
                }
            }
        }

        for y in 0..height {
            let next = std::cmp::min(y + half_kernel, height - 1) as usize * src_stride as usize;
            let previous =
                std::cmp::max(y as i64 - half_kernel as i64, 0) as usize * src_stride as usize;
            let y_dst_shift = dst_stride as usize * y as usize;
            // Prune previous and add next and compute mean

            kernel[0] += unsafe { *src.get_unchecked(next + px) }.as_();
            kernel[1] += unsafe { *src.get_unchecked(next + px + 1) }.as_();
            kernel[2] += unsafe { *src.get_unchecked(next + px + 2) }.as_();

            kernel[0] -= unsafe { *src.get_unchecked(previous + px) }.as_();
            kernel[1] -= unsafe { *src.get_unchecked(previous + px + 1) }.as_();
            kernel[2] -= unsafe { *src.get_unchecked(previous + px + 2) }.as_();

            match box_channels {
                FastBlurChannels::Channels3 => {}
                FastBlurChannels::Channels4 => {
                    kernel[3] += unsafe { *src.get_unchecked(next + px + 3) }.as_();
                    kernel[3] -= unsafe { *src.get_unchecked(previous + px + 3) }.as_();
                }
            }

            let write_offset = y_dst_shift + px;
            unsafe {
                if USE_ROUNDING {
                    unsafe_dst.write(
                        write_offset + 0,
                        T::from_f32((kernel[0].as_() * weight).round()).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 1,
                        T::from_f32((kernel[1].as_() * weight).round()).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 2,
                        T::from_f32((kernel[2].as_() * weight).round()).unwrap_or_default(),
                    );

                    match box_channels {
                        FastBlurChannels::Channels3 => {}
                        FastBlurChannels::Channels4 => {
                            unsafe_dst.write(
                                write_offset + 3,
                                T::from_f32((kernel[3].as_() * weight).round()).unwrap_or_default(),
                            );
                        }
                    }
                } else {
                    unsafe_dst.write(
                        write_offset + 0,
                        T::from_f32(kernel[0].as_() * weight).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 1,
                        T::from_f32(kernel[1].as_() * weight).unwrap_or_default(),
                    );
                    unsafe_dst.write(
                        write_offset + 2,
                        T::from_f32(kernel[2].as_() * weight).unwrap_or_default(),
                    );

                    match box_channels {
                        FastBlurChannels::Channels3 => {}
                        FastBlurChannels::Channels4 => {
                            unsafe_dst.write(
                                write_offset + 3,
                                T::from_f32(kernel[3].as_() * weight).unwrap_or_default(),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn box_blur_vertical_pass<
    T: FromPrimitive + Default + Sync + Send + Copy,
    const CHANNEL_CONFIGURATION: usize,
>(
    src: &[T],
    src_stride: u32,
    dst: &mut [T],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    pool: &ThreadPool,
    thread_count: u32,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + AsPrimitive<u32>
        + AsPrimitive<u64>
        + AsPrimitive<f32>
        + AsPrimitive<f64>,
{
    let mut _dispatcher_vertical: fn(
        src: &[T],
        src_stride: u32,
        unsafe_dst: &UnsafeSlice<T>,
        dst_stride: u32,
        width: u32,
        height: u32,
        radius: u32,
        start_x: u32,
        end_x: u32,
    ) = box_blur_vertical_pass_impl::<T, u32, CHANNEL_CONFIGURATION, false>;
    if std::any::type_name::<T>() == "u8" || std::any::type_name::<T>() == "u16" {
        _dispatcher_vertical = box_blur_vertical_pass_impl::<T, u32, CHANNEL_CONFIGURATION, true>;
    } else if std::any::type_name::<T>() == "f32" {
        _dispatcher_vertical = box_blur_vertical_pass_impl::<T, f32, CHANNEL_CONFIGURATION, false>;
    }
    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "x86"),
        target_feature = "sse4.1"
    ))]
    {
        if std::any::type_name::<T>() == "u8" {
            _dispatcher_vertical =
                sse_support::box_blur_vertical_pass_sse::<T, CHANNEL_CONFIGURATION>;
        }
    }
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        if std::any::type_name::<T>() == "u8" {
            _dispatcher_vertical =
                neon_support::box_blur_vertical_pass_neon::<T, CHANNEL_CONFIGURATION>;
        }
    }
    let unsafe_dst = UnsafeSlice::new(dst);

    pool.scope(|scope| {
        let segment_size = width / thread_count;
        for i in 0..thread_count {
            let start_x = i * segment_size;
            let mut end_x = (i + 1) * segment_size;
            if i == thread_count - 1 {
                end_x = width;
            }

            scope.spawn(move |_| {
                _dispatcher_vertical(
                    src,
                    src_stride,
                    &unsafe_dst,
                    dst_stride,
                    width,
                    height,
                    radius,
                    start_x,
                    end_x,
                );
            });
        }
    });
}

fn box_blur_impl<
    T: FromPrimitive + Default + Sync + Send + Copy,
    const CHANNEL_CONFIGURATION: usize,
>(
    src: &[T],
    src_stride: u32,
    dst: &mut [T],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    pool: &ThreadPool,
    thread_count: u32,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + AsPrimitive<u32>
        + AsPrimitive<u64>
        + AsPrimitive<f32>
        + AsPrimitive<f64>,
{
    let mut transient: Vec<T> =
        vec![T::from_u32(0).unwrap_or_default(); dst_stride as usize * height as usize];
    box_blur_horizontal_pass::<T, CHANNEL_CONFIGURATION>(
        src,
        src_stride,
        &mut transient,
        dst_stride,
        width,
        height,
        radius,
        pool,
        thread_count,
    );
    box_blur_vertical_pass::<T, CHANNEL_CONFIGURATION>(
        &transient,
        src_stride,
        dst,
        dst_stride,
        width,
        height,
        radius,
        pool,
        thread_count,
    );
}

/// Performs box blur on the image.
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn box_blur(
    src: &[u8],
    src_stride: u32,
    dst: &mut [u8],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let thread_count = threading_policy.get_threads_count(width, height) as u32;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count as usize)
        .build()
        .unwrap();
    match channels {
        FastBlurChannels::Channels3 => {
            box_blur_impl::<u8, 3>(
                src,
                src_stride,
                dst,
                dst_stride,
                width,
                height,
                radius,
                &pool,
                thread_count,
            );
        }
        FastBlurChannels::Channels4 => {
            box_blur_impl::<u8, 4>(
                src,
                src_stride,
                dst,
                dst_stride,
                width,
                height,
                radius,
                &pool,
                thread_count,
            );
        }
    }
}

/// Performs box blur on the image.
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn box_blur_u16(
    src: &[u16],
    dst: &mut [u16],
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let stride = width * channels.get_channels() as u32;
    let thread_count = threading_policy.get_threads_count(width, height) as u32;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count as usize)
        .build()
        .unwrap();
    match channels {
        FastBlurChannels::Channels3 => {
            box_blur_impl::<u16, 3>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                &pool,
                thread_count,
            );
        }
        FastBlurChannels::Channels4 => {
            box_blur_impl::<u16, 4>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                &pool,
                thread_count,
            );
        }
    }
}

/// Performs box blur on the image.
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn box_blur_f32(
    src: &[f32],
    dst: &mut [f32],
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let stride = width * channels.get_channels() as u32;
    let thread_count = threading_policy.get_threads_count(width, height) as u32;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count as usize)
        .build()
        .unwrap();
    match channels {
        FastBlurChannels::Channels3 => {
            box_blur_impl::<f32, 3>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                &pool,
                thread_count,
            );
        }
        FastBlurChannels::Channels4 => {
            box_blur_impl::<f32, 4>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                &pool,
                thread_count,
            );
        }
    }
}

/// Performs box blur on the image in linear colorspace
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels of the image, only 3 and 4 is supported, alpha position, and channels order does not matter
/// * `threading_policy` - Threads usage policy
/// * `transfer_function` - Transfer function in linear colorspace
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn box_blur_in_linear(
    src: &[u8],
    src_stride: u32,
    dst: &mut [u8],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
    transfer_function: TransferFunction,
) {
    let mut linear_data: Vec<f32> =
        vec![0f32; width as usize * height as usize * channels.get_channels()];
    let mut linear_data_2: Vec<f32> =
        vec![0f32; width as usize * height as usize * channels.get_channels()];

    let forward_transformer = match channels {
        FastBlurChannels::Channels3 => rgb_to_linear,
        FastBlurChannels::Channels4 => rgba_to_linear,
    };

    let inverse_transformer = match channels {
        FastBlurChannels::Channels3 => linear_to_rgb,
        FastBlurChannels::Channels4 => linear_to_rgba,
    };

    forward_transformer(
        &src,
        src_stride,
        &mut linear_data,
        width * std::mem::size_of::<f32>() as u32 * channels.get_channels() as u32,
        width,
        height,
        transfer_function,
    );

    box_blur_f32(
        &linear_data,
        &mut linear_data_2,
        width,
        height,
        radius,
        channels,
        threading_policy,
    );

    inverse_transformer(
        &linear_data_2,
        width * std::mem::size_of::<f32>() as u32 * channels.get_channels() as u32,
        dst,
        dst_stride,
        width,
        height,
        transfer_function,
    );
}

fn tent_blur_impl<
    T: FromPrimitive + Default + Sync + Send + Copy,
    const CHANNEL_CONFIGURATION: usize,
>(
    src: &[T],
    src_stride: u32,
    dst: &mut [T],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    threading_policy: ThreadingPolicy,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + AsPrimitive<u32>
        + AsPrimitive<u64>
        + AsPrimitive<f32>
        + AsPrimitive<f64>,
{
    let thread_count = threading_policy.get_threads_count(width, height) as u32;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count as usize)
        .build()
        .unwrap();
    let mut transient: Vec<T> =
        vec![T::from_u32(0).unwrap_or_default(); dst_stride as usize * height as usize];
    box_blur_impl::<T, CHANNEL_CONFIGURATION>(
        src,
        src_stride,
        &mut transient,
        dst_stride,
        width,
        height,
        radius,
        &pool,
        thread_count,
    );
    box_blur_impl::<T, CHANNEL_CONFIGURATION>(
        &transient,
        src_stride,
        dst,
        dst_stride,
        width,
        height,
        radius,
        &pool,
        thread_count,
    );
}

/// Performs tent blur on the image.
///
/// Tent blur just makes a two passes box blur on the image since two times box it is almost equal to tent filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn tent_blur(
    src: &[u8],
    src_stride: u32,
    dst: &mut [u8],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    match channels {
        FastBlurChannels::Channels3 => {
            tent_blur_impl::<u8, 3>(
                src,
                src_stride,
                dst,
                dst_stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
        FastBlurChannels::Channels4 => {
            tent_blur_impl::<u8, 4>(
                src,
                src_stride,
                dst,
                dst_stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
    }
}

/// Performs tent blur on the image.
///
/// Tent blur just makes a two passes box blur on the image since two times box it is almost equal to tent filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn tent_blur_u16(
    src: &[u16],
    dst: &mut [u16],
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let stride = width * channels.get_channels() as u32;
    match channels {
        FastBlurChannels::Channels3 => {
            tent_blur_impl::<u16, 3>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
        FastBlurChannels::Channels4 => {
            tent_blur_impl::<u16, 4>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
    }
}

/// Performs tent blur on the image.
///
/// Tent blur just makes a two passes box blur on the image since two times box it is almost equal to tent filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn tent_blur_f32(
    src: &[f32],
    dst: &mut [f32],
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let stride = width * channels.get_channels() as u32;
    match channels {
        FastBlurChannels::Channels3 => {
            tent_blur_impl::<f32, 3>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
        FastBlurChannels::Channels4 => {
            tent_blur_impl::<f32, 4>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
    }
}

/// Performs tent blur on the image in linear colorspace
///
/// Tent blur just makes a two passes box blur on the image since two times box it is almost equal to tent filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels of the image, only 3 and 4 is supported, alpha position, and channels order does not matter
/// * `threading_policy` - Threads usage policy
/// * `transfer_function` - Transfer function in linear colorspace
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn tent_blur_in_linear(
    src: &[u8],
    src_stride: u32,
    dst: &mut [u8],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
    transfer_function: TransferFunction,
) {
    let mut linear_data: Vec<f32> =
        vec![0f32; width as usize * height as usize * channels.get_channels()];
    let mut linear_data_2: Vec<f32> =
        vec![0f32; width as usize * height as usize * channels.get_channels()];

    let forward_transformer = match channels {
        FastBlurChannels::Channels3 => rgb_to_linear,
        FastBlurChannels::Channels4 => rgba_to_linear,
    };

    let inverse_transformer = match channels {
        FastBlurChannels::Channels3 => linear_to_rgb,
        FastBlurChannels::Channels4 => linear_to_rgba,
    };

    forward_transformer(
        &src,
        src_stride,
        &mut linear_data,
        width * std::mem::size_of::<f32>() as u32 * channels.get_channels() as u32,
        width,
        height,
        transfer_function,
    );

    tent_blur_f32(
        &linear_data,
        &mut linear_data_2,
        width,
        height,
        radius,
        channels,
        threading_policy,
    );

    inverse_transformer(
        &linear_data_2,
        width * std::mem::size_of::<f32>() as u32 * channels.get_channels() as u32,
        dst,
        dst_stride,
        width,
        height,
        transfer_function,
    );
}

fn gaussian_box_blur_impl<
    T: FromPrimitive + Default + Sync + Send + Copy,
    const CHANNEL_CONFIGURATION: usize,
>(
    src: &[T],
    src_stride: u32,
    dst: &mut [T],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    threading_policy: ThreadingPolicy,
) where
    T: std::ops::AddAssign
        + std::ops::SubAssign
        + Copy
        + AsPrimitive<u32>
        + AsPrimitive<u64>
        + AsPrimitive<f32>
        + AsPrimitive<f64>,
{
    let thread_count = threading_policy.get_threads_count(width, height) as u32;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count as usize)
        .build()
        .unwrap();
    let mut transient: Vec<T> =
        vec![T::from_u32(0).unwrap_or_default(); dst_stride as usize * height as usize];
    let mut transient2: Vec<T> =
        vec![T::from_u32(0).unwrap_or_default(); dst_stride as usize * height as usize];
    box_blur_impl::<T, CHANNEL_CONFIGURATION>(
        &src,
        src_stride,
        &mut transient,
        dst_stride,
        width,
        height,
        radius,
        &pool,
        thread_count,
    );
    box_blur_impl::<T, CHANNEL_CONFIGURATION>(
        &transient,
        src_stride,
        &mut transient2,
        dst_stride,
        width,
        height,
        radius,
        &pool,
        thread_count,
    );
    box_blur_impl::<T, CHANNEL_CONFIGURATION>(
        &transient2,
        src_stride,
        dst,
        dst_stride,
        width,
        height,
        radius,
        &pool,
        thread_count,
    );
}

/// Performs gaussian box blur approximation on the image.
///
/// This method launches three times box blur on the image since 2 passes box filter it is a tent filter and 3 passes of box blur it is almost gaussian filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// Even it is having low complexity it is slow filter.
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn gaussian_box_blur(
    src: &[u8],
    src_stride: u32,
    dst: &mut [u8],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    match channels {
        FastBlurChannels::Channels3 => {
            gaussian_box_blur_impl::<u8, 3>(
                src,
                src_stride,
                dst,
                dst_stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
        FastBlurChannels::Channels4 => {
            gaussian_box_blur_impl::<u8, 4>(
                src,
                src_stride,
                dst,
                dst_stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
    }
}

/// Performs gaussian box blur approximation on the image.
///
/// This method launches three times box blur on the image since 2 passes box filter it is a tent filter and 3 passes of box blur it is almost gaussian filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// Even it is having low complexity it is slow filter.
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn gaussian_box_blur_u16(
    src: &[u16],
    dst: &mut [u16],
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let stride = width * channels.get_channels() as u32;
    match channels {
        FastBlurChannels::Channels3 => {
            gaussian_box_blur_impl::<u16, 3>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
        FastBlurChannels::Channels4 => {
            gaussian_box_blur_impl::<u16, 4>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
    }
}

/// Performs gaussian box blur approximation on the image.
///
/// This method launches three times box blur on the image since 2 passes box filter it is a tent filter and 3 passes of box blur it is almost gaussian filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// Even it is having low complexity it is slow filter.
/// O(1) complexity.
///
/// # Arguments
///
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels in the image
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn gaussian_box_blur_f32(
    src: &[f32],
    dst: &mut [f32],
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
) {
    let stride = width * channels.get_channels() as u32;
    match channels {
        FastBlurChannels::Channels3 => {
            gaussian_box_blur_impl::<f32, 3>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
        FastBlurChannels::Channels4 => {
            gaussian_box_blur_impl::<f32, 4>(
                src,
                stride,
                dst,
                stride,
                width,
                height,
                radius,
                threading_policy,
            );
        }
    }
}

/// Performs gaussian box blur approximation on the image.
///
/// This method launches three times box blur on the image since 2 passes box filter it is a tent filter and 3 passes of box blur it is almost gaussian filter.
/// https://en.wikipedia.org/wiki/Central_limit_theorem
///
/// Convergence of this function is very high so strong effect applies very fast
///
/// O(1) complexity.
///
/// # Arguments
///
/// * `stride` - Lane length, default is width * channels_count if not aligned
/// * `width` - Width of the image
/// * `height` - Height of the image
/// * `radius` - almost any radius is supported
/// * `channels` - Count of channels of the image, only 3 and 4 is supported, alpha position, and channels order does not matter
/// * `threading_policy` - Threads usage policy
/// * `transfer_function` - Transfer function in linear colorspace
///
/// # Panics
/// Panic is stride/width/height/channel configuration do not match provided
pub fn gaussian_box_blur_in_linear(
    src: &[u8],
    src_stride: u32,
    dst: &mut [u8],
    dst_stride: u32,
    width: u32,
    height: u32,
    radius: u32,
    channels: FastBlurChannels,
    threading_policy: ThreadingPolicy,
    transfer_function: TransferFunction,
) {
    let mut linear_data: Vec<f32> =
        vec![0f32; width as usize * height as usize * channels.get_channels()];
    let mut linear_data_2: Vec<f32> =
        vec![0f32; width as usize * height as usize * channels.get_channels()];

    let forward_transformer = match channels {
        FastBlurChannels::Channels3 => rgb_to_linear,
        FastBlurChannels::Channels4 => rgba_to_linear,
    };

    let inverse_transformer = match channels {
        FastBlurChannels::Channels3 => linear_to_rgb,
        FastBlurChannels::Channels4 => linear_to_rgba,
    };

    forward_transformer(
        &src,
        src_stride,
        &mut linear_data,
        width * std::mem::size_of::<f32>() as u32 * channels.get_channels() as u32,
        width,
        height,
        transfer_function,
    );

    gaussian_box_blur_f32(
        &linear_data,
        &mut linear_data_2,
        width,
        height,
        radius,
        channels,
        threading_policy,
    );

    inverse_transformer(
        &linear_data_2,
        width * std::mem::size_of::<f32>() as u32 * channels.get_channels() as u32,
        dst,
        dst_stride,
        width,
        height,
        transfer_function,
    );
}
