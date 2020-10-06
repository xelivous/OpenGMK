use crate::{game::string::RCStr, math::Real};
use gmio::render::AtlasRef;
use image::{Pixel, RgbaImage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Sprite {
    pub name: RCStr,
    pub frames: Vec<Frame>,
    pub colliders: Vec<Collider>,
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub per_frame_colliders: bool,
    pub bbox_left: u32,
    pub bbox_right: u32,
    pub bbox_top: u32,
    pub bbox_bottom: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub atlas_ref: AtlasRef,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct BoundingBox {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Collider {
    pub width: u32,
    pub height: u32,
    pub bbox_left: u32,
    pub bbox_right: u32,
    pub bbox_top: u32,
    pub bbox_bottom: u32,
    pub data: Box<[bool]>,
}

pub fn process_image(image: &mut RgbaImage, removeback: bool, smooth: bool) {
    if removeback {
        // remove background colour
        let bottom_left = image.get_pixel(0, image.height() - 1).to_rgb();
        for px in image.pixels_mut() {
            if px.to_rgb() == bottom_left {
                px[3] = 0;
            }
        }
    }
    if smooth {
        // smooth
        for y in 0..image.height() {
            for x in 0..image.width() {
                // if pixel is transparent
                if image.get_pixel(x, y)[3] == 0 {
                    // for all surrounding pixels
                    for y in y.saturating_sub(1)..(y + 2).min(image.height()) {
                        for x in x.saturating_sub(1)..(x + 2).min(image.width()) {
                            // subtract 32 if possible
                            if image.get_pixel(x, y)[3] >= 32 {
                                image.get_pixel_mut(x, y)[3] -= 32;
                            }
                        }
                    }
                }
            }
        }
    }
    if removeback {
        // make lerping less ugly
        for y in 0..image.height() {
            for x in 0..image.width() {
                if image.get_pixel(x, y)[3] == 0 {
                    let (sx, sy) = if x > 0 && image.get_pixel(x - 1, y)[3] != 0 {
                        (x - 1, y)
                    } else if x < image.width() - 1 && image.get_pixel(x + 1, y)[3] != 0 {
                        (x + 1, y)
                    } else if y > 0 && image.get_pixel(x, y - 1)[3] != 0 {
                        (x, y - 1)
                    } else if y < image.height() - 1 && image.get_pixel(x, y + 1)[3] != 0 {
                        (x, y + 1)
                    } else {
                        continue
                    };
                    let src = *image.get_pixel(sx, sy);
                    let dst = image.get_pixel_mut(x, y);
                    dst[0] = src[0];
                    dst[1] = src[1];
                    dst[2] = src[2];
                }
            }
        }
    }
}

/// Calculates bounding box values for a given frame.
/// The algorithm doesn't check more pixels than it needs to.
fn make_bbox(coll: impl Fn(u32, u32) -> bool, frame_width: u32, frame_height: u32) -> BoundingBox {
    let mut left = frame_width - 1;
    let mut right = 0;
    let mut top = frame_height - 1;
    let mut bottom = 0;
    // Set bbox_left and bbox_top to the leftmost column with collision, and the highest pixel within that column.
    for x in 0..frame_width {
        if let Some(y) = (0..frame_height).find(|&y| coll(x, y)) {
            left = x;
            top = y;
            break
        }
    }
    // Set bbox_top to the highest pixel in the remaining columns, if there's one above the one we already found.
    if let Some(y) = (0..top).find(|&y| ((left + 1)..frame_width).any(|x| coll(x, y))) {
        top = y;
    }
    // Set bbox_right and bbox_bottom to the rightmost column with collision, and the lowest pixel within that column,
    // ignoring the rows and columns which are known to be empty.
    for x in (left..frame_width).rev() {
        if let Some(y) = (top..frame_height).rfind(|&y| coll(x, y)) {
            right = x;
            bottom = y;
            break
        }
    }
    // Set bbox_bottom to the lowest pixel between bbox_left and bbox_right, if there's one below the one we found.
    if let Some(y) = ((bottom + 1)..frame_height).rev().find(|&y| (left..(right + 1)).any(|x| coll(x, y))) {
        bottom = y;
    }
    BoundingBox { left, right, top, bottom }
}

/// Creates a collider from the given collision data and dimensions, giving it an appropriate bounding box.
fn complete_bbox(data: Box<[bool]>, width: u32, height: u32) -> Collider {
    let bbox = make_bbox(|x, y| data[(y * width + x) as usize], width, height);
    Collider {
        width,
        height,
        bbox_left: bbox.left,
        bbox_right: bbox.right,
        bbox_top: bbox.top,
        bbox_bottom: bbox.bottom,
        data,
    }
}

pub fn make_colliders_precise(frames: &[RgbaImage], tolerance: u8, sepmasks: bool) -> Vec<Collider> {
    let width = frames[0].width();
    let height = frames[0].height();
    if sepmasks {
        frames
            .iter()
            .map(|f| {
                complete_bbox(
                    f.pixels().map(|p| p[3] > tolerance).collect::<Vec<_>>().into_boxed_slice(),
                    width,
                    height,
                )
            })
            .collect()
    } else {
        let mut data = vec![false; (width * height) as usize];
        // merge pixels
        for f in frames {
            for y in 0..height.min(f.height()) {
                for x in 0..width.min(f.width()) {
                    if f.get_pixel(x, y)[3] > tolerance {
                        data[(y * width + x) as usize] = true;
                    }
                }
            }
        }
        vec![complete_bbox(data.into_boxed_slice(), width, height)]
    }
}

// used for adding frames to sprites
pub fn scale(input: &mut RgbaImage, width: u32, height: u32) {
    if input.dimensions() != (width, height) {
        let xscale = Real::from(width) / input.width().into();
        let yscale = Real::from(height) / input.height().into();
        let mut output_vec = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let px = input.get_pixel(
                    (Real::from(x) / xscale).floor().round() as _,
                    (Real::from(y) / yscale).floor().round() as _,
                );
                // this makes lerping uglier but it's accurate to GM8
                if px[3] > 0 {
                    output_vec.extend_from_slice(px.channels());
                } else {
                    output_vec.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        *input = RgbaImage::from_vec(width, height, output_vec).unwrap();
    }
}
