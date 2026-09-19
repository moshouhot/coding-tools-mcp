//! 托盘图标：只有两种状态。
//!
//! - **运行中**：原作者的图标原样，直接复用 Tauri 的默认窗口图标。
//! - **未运行**：同一图标转灰度（保留 alpha）。
//!
//! 灰度版本只按源图计算一次并缓存；源图不可用时回退到编译期嵌入的 PNG。

use std::sync::OnceLock;

use tauri::image::Image;
use tauri::AppHandle;

/// 回退源图边长。
const FALLBACK_SIZE: u32 = 128;

/// 编译期嵌入，仅在拿不到默认窗口图标时使用。
const SOURCE_PNG: &[u8] = include_bytes!("../../icons/128x128.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 至少一个工作区的 MCP 实际在监听。
    Running,
    /// 没有任何 MCP 在运行。
    Stopped,
}

/// 灰度化：按亮度加权，alpha 原样保留。
///
/// 只改颜色、不改形状，因此两种状态的轮廓完全一致。
fn grayscale(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|px| {
            // Rec.601 亮度权重，整数运算避免浮点误差。
            let luma =
                ((299 * px[0] as u32 + 587 * px[1] as u32 + 114 * px[2] as u32) / 1000) as u8;
            [luma, luma, luma, px[3]]
        })
        .collect()
}

/// 源图 RGBA 与尺寸：优先用默认窗口图标，取不到时回退嵌入 PNG。
fn source(app: &AppHandle) -> (Vec<u8>, u32, u32) {
    if let Some(icon) = app.default_window_icon() {
        return (icon.rgba().to_vec(), icon.width(), icon.height());
    }
    let decoded = image::load_from_memory_with_format(SOURCE_PNG, image::ImageFormat::Png)
        .expect("embedded tray icon must decode")
        .to_rgba8();
    let (w, h) = decoded.dimensions();
    debug_assert_eq!(
        (w, h),
        (FALLBACK_SIZE, FALLBACK_SIZE),
        "回退源图尺寸与预期不符"
    );
    (decoded.into_raw(), w, h)
}

/// 运行中：原图标原样（与原作者一致）。
pub fn running_icon(app: &AppHandle) -> Image<'static> {
    let (rgba, w, h) = source(app);
    Image::new_owned(rgba, w, h)
}

/// 未运行：原图标的灰度版本，只计算一次。
pub fn stopped_icon(app: &AppHandle) -> Image<'static> {
    static STOPPED: OnceLock<Image<'static>> = OnceLock::new();
    STOPPED
        .get_or_init(|| {
            let (rgba, w, h) = source(app);
            Image::new_owned(grayscale(&rgba), w, h)
        })
        .clone()
}

/// 按档位取图标。
pub fn image_for(app: &AppHandle, level: Level) -> Image<'static> {
    match level {
        Level::Running => running_icon(app),
        Level::Stopped => stopped_icon(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立构造一张小图，避免测试依赖 Tauri 运行时。
    fn sample() -> Vec<u8> {
        vec![
            255, 0, 0, 255, // 红
            0, 255, 0, 128, // 半透明绿
            0, 0, 0, 0, // 全透明
            255, 255, 255, 255, // 白
        ]
    }

    #[test]
    fn grayscale_keeps_length_and_alpha() {
        let src = sample();
        let gray = grayscale(&src);
        assert_eq!(gray.len(), src.len());
        let alpha_src: Vec<u8> = src.chunks_exact(4).map(|px| px[3]).collect();
        let alpha_gray: Vec<u8> = gray.chunks_exact(4).map(|px| px[3]).collect();
        assert_eq!(alpha_src, alpha_gray, "灰度化不应改变透明度");
    }

    #[test]
    fn grayscale_output_is_actually_gray() {
        for px in grayscale(&sample()).chunks_exact(4) {
            assert_eq!(px[0], px[1], "像素不是灰度：{px:?}");
            assert_eq!(px[1], px[2], "像素不是灰度：{px:?}");
        }
    }

    #[test]
    fn grayscale_is_monotonic_in_brightness() {
        // 黑 < 半透明绿 < 白，灰度值必须保持这个顺序。
        let gray = grayscale(&sample());
        let luma = |i: usize| gray[i * 4];
        assert!(luma(2) < luma(1), "黑色应比绿色暗");
        assert!(luma(1) < luma(3), "绿色应比白色暗");
    }

    #[test]
    fn embedded_fallback_has_expected_size() {
        let decoded = image::load_from_memory_with_format(SOURCE_PNG, image::ImageFormat::Png)
            .expect("embedded tray icon must decode")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (FALLBACK_SIZE, FALLBACK_SIZE));
    }

    #[test]
    fn embedded_fallback_is_visible_but_not_full_coverage() {
        let decoded = image::load_from_memory_with_format(SOURCE_PNG, image::ImageFormat::Png)
            .expect("decode")
            .to_rgba8();
        let visible = decoded.pixels().filter(|px| px.0[3] > 0).count();
        let total = (FALLBACK_SIZE * FALLBACK_SIZE) as usize;
        assert!(visible > 100, "可见像素过少（{visible}）");
        assert!(visible < total, "应留出透明区域，实际覆盖 {visible}/{total}");
    }
}
