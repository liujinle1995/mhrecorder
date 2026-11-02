//! 图像基础操作：解码与 Mat 转换。
//!
//! 各模块都要用，又都不该自己实现一遍。

use crate::prelude::*;
// ---------------------------------------------------------------------------
// Image decoding
// ---------------------------------------------------------------------------

/// Decode raw file bytes (PNG/JPEG/BMP/WebP) into RGBA pixels.
pub fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage> {
    let dyn_img = image::load_from_memory(bytes)
        .map_err(|e| VisionError::Decode(e.to_string()))?;
    Ok(dyn_img.to_rgba8())
}

/// Convert an RGBA image into an OpenCV BGR `Mat`.
pub(crate) fn rgba_to_bgr_mat(img: &image::RgbaImage) -> Result<opencv::core::Mat> {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut bgr = Vec::with_capacity((w * h * 3) as usize);
    for p in img.pixels() {
        // image gives RGBA; OpenCV's default channel order is BGR.
        bgr.push(p[2]);
        bgr.push(p[1]);
        bgr.push(p[0]);
    }
    let flat = opencv::core::Mat::from_slice(&bgr)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    // `from_slice` borrows the Vec, so clone into an owned Mat and reshape it.
    let owned = flat
        .try_clone()
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    let mut out = opencv::core::Mat::default();
    owned
        .reshape(3, h)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?
        .copy_to(&mut out)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    if out.cols() != w || out.rows() != h {
        return Err(VisionError::OpenCv(format!(
            "mat shape mismatch: got {}x{}, wanted {w}x{h}",
            out.cols(),
            out.rows()
        )));
    }
    Ok(out)
}

/// Convert an OpenCV grayscale `Mat` into a plain Vec<u8> of length w*h.
pub(crate) fn gray_mat_to_vec(mat: &opencv::core::Mat) -> Result<(Vec<u8>, i32, i32)> {
    let rows = mat.rows();
    let cols = mat.cols();
    // Mat::data_bytes is the safest accessor for a CV_8U single-channel Mat.
    let bytes = mat.data_bytes().map_err(|e| VisionError::OpenCv(e.to_string()))?;
    debug_assert!(bytes.len() >= (rows * cols) as usize);
    Ok((bytes[..(rows * cols) as usize].to_vec(), rows, cols))
}

pub(crate) fn to_gray(mat: &opencv::core::Mat) -> Result<opencv::core::Mat> {
    let mut gray = opencv::core::Mat::default();
    opencv::imgproc::cvt_color_def(mat, &mut gray, opencv::imgproc::COLOR_BGR2GRAY)
        .map_err(|e| VisionError::OpenCv(e.to_string()))?;
    Ok(gray)
}
