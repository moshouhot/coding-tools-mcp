//! 托盘图标的颜色变体：把应用图标的蓝色字形重新着色为状态色。
//!
//! 只保留“明显偏蓝”的字形像素（排除浅色底板与圆角），把颜色换成状态色后
//! 按 4x4 盒式滤波降采样到 32x32，保证在 Windows 托盘 16px 下仍可辨认。
//! 三个变体只解码/计算一次并缓存。

use std::sync::OnceLock;

use tauri::image::Image;

/// 状态色。灰色表示未运行，绿色表示正常，黄色表示需要注意。
pub const COLOR_STOPPED: (u8, u8, u8) = (130, 135, 142);
pub const COLOR_OK: (u8, u8, u8) = (34, 160, 60);
pub const COLOR_WARN: (u8, u8, u8) = (230, 160, 20);

/// 托盘图标边长。Windows 托盘实际显示 16px，32px 留出高 DPI 余量。
const OUT_SIZE: u32 = 32;
/// 源图边长。
const SRC_SIZE: u32 = 128;

/// 编译期嵌入，避免运行时依赖资源路径（开发态与安装后一致）。
const SOURCE_PNG: &[u8] = include_bytes!("../../icons/128x128.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Stopped,
    Ok,
    Warn,
}

impl Level {
    fn color(self) -> (u8, u8, u8) {
        match self {
            Level::Stopped => COLOR_STOPPED,
            Level::Ok => COLOR_OK,
            Level::Warn => COLOR_WARN,
        }
    }
}

/// 源图的字形 alpha 蒙版（128x128，行优先）。
fn mask() -> &'static Vec<u8> {
    static MASK: OnceLock<Vec<u8>> = OnceLock::new();
    MASK.get_or_init(|| {
        let decoded = image::load_from_memory_with_format(SOURCE_PNG, image::ImageFormat::Png)
            .expect("embedded tray icon must decode");
        let rgba = decoded.to_rgba8();
        let (w, h) = rgba.dimensions();
        assert_eq!(
            (w, h),
            (SRC_SIZE, SRC_SIZE),
            "tray icon source must be {SRC_SIZE}x{SRC_SIZE}"
        );
        let mut out = vec![0u8; (SRC_SIZE * SRC_SIZE) as usize];
        for (index, pixel) in rgba.pixels().enumerate() {
            let [r, g, b, a] = pixel.0;
            // 只取饱和的蓝色字形，排除近白底板与浅蓝圆角。
            let is_glyph = a > 0 && b.saturating_sub(r) > 60 && b.saturating_sub(g) > 40 && r < 190;
            out[index] = if is_glyph { a } else { 0 };
        }
        out
    })
}

/// 把 128x128 蒙版按 4x4 平均降采样到 32x32。
fn downsampled_mask() -> &'static Vec<u8> {
    static SMALL: OnceLock<Vec<u8>> = OnceLock::new();
    SMALL.get_or_init(|| {
        let src = mask();
        let factor = SRC_SIZE / OUT_SIZE;
        let mut out = vec![0u8; (OUT_SIZE * OUT_SIZE) as usize];
        for y in 0..OUT_SIZE {
            for x in 0..OUT_SIZE {
                let mut sum = 0u32;
                for dy in 0..factor {
                    for dx in 0..factor {
                        let sx = x * factor + dx;
                        let sy = y * factor + dy;
                        sum += src[(sy * SRC_SIZE + sx) as usize] as u32;
                    }
                }
                out[(y * OUT_SIZE + x) as usize] = (sum / (factor * factor)) as u8;
            }
        }
        out
    })
}

/// 生成指定状态色的 RGBA 像素。
pub fn rgba_for(level: Level) -> Vec<u8> {
    let (r, g, b) = level.color();
    downsampled_mask()
        .iter()
        .flat_map(|&a| [r, g, b, a])
        .collect()
}

/// 取（并缓存）托盘图标。
pub fn image_for(level: Level) -> Image<'static> {
    static CACHE: OnceLock<Vec<(Level, Image<'static>)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        [Level::Stopped, Level::Ok, Level::Warn]
            .into_iter()
            .map(|level| {
                let rgba = rgba_for(level);
                (level, Image::new_owned(rgba, OUT_SIZE, OUT_SIZE))
            })
            .collect()
    });
    cache
        .iter()
        .find(|(cached, _)| *cached == level)
        .map(|(_, image)| image.clone())
        .expect("all three levels are cached")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_mask_is_non_trivial_and_centered() {
        let small = downsampled_mask();
        let visible = small.iter().filter(|&&a| a > 0).count();
        assert!(
            visible > 100,
            "字形蒙版可见像素过少（{visible}），重新着色可能选错了区域"
        );
        assert!(
            visible < (OUT_SIZE * OUT_SIZE) as usize / 2,
            "字形蒙版覆盖过大（{visible}），可能把底板也算进去了"
        );
    }

    #[test]
    fn corners_are_transparent() {
        let small = downsampled_mask();
        let last = OUT_SIZE - 1;
        for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
            assert_eq!(
                small[(y * OUT_SIZE + x) as usize], 0,
                "角点 ({x},{y}) 必须完全透明"
            );
        }
    }

    #[test]
    fn levels_produce_distinct_opaque_colors() {
        let stopped = rgba_for(Level::Stopped);
        let ok = rgba_for(Level::Ok);
        let warn = rgba_for(Level::Warn);
        assert_eq!(stopped.len(), (OUT_SIZE * OUT_SIZE * 4) as usize);
        assert_ne!(stopped, ok);
        assert_ne!(ok, warn);
        assert_ne!(stopped, warn);

        let opaque = |buf: &[u8]| {
            buf.chunks_exact(4)
                .filter(|px| px[3] > 0)
                .map(|px| (px[0], px[1], px[2]))
                .collect::<std::collections::HashSet<_>>()
        };
        assert_eq!(opaque(&ok), std::collections::HashSet::from([COLOR_OK]));
        assert_eq!(
            opaque(&warn),
            std::collections::HashSet::from([COLOR_WARN])
        );
    }

    #[test]
    fn image_dimensions_match_out_size() {
        let image = image_for(Level::Ok);
        assert_eq!(image.width(), OUT_SIZE);
        assert_eq!(image.height(), OUT_SIZE);
    }
}
