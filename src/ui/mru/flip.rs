//! Flip 3D: the recent-windows switcher as a stack of tilted windows receding
//! into the screen, as Windows Vista's Flip 3D.
//!
//! Pure geometry, tested without a renderer: a window's pose in the scene,
//! the camera that projects it onto the output, the homography that maps the
//! window onto its projected quad, and where each window sits in the stack as
//! the selection scrolls through it.
//!
//! The scene's x and y are output-logical coordinates on the screen plane
//! (z = 0), z grows away from the viewer. A window lying flat at z = 0 projects
//! exactly onto its own rectangle, so the switcher can open from, and close
//! back onto, where the windows really are. The camera looks from the front
//! window's centre, or from beside it (`camera-x`, `camera-y`: from above,
//! as Vista's Flip 3D, the stack rises towards the top).
//!
//! Every window faces the same way (turned left or right, then up or down),
//! and each one sits straight behind the one in front of it, so the stack
//! goes back opposite to where the windows face: facing right and down, it
//! recedes left and up. Each one can also be shifted from the one in front,
//! along the windows' own right and down directions as they lie on the screen
//! (the offset), so it turns with them, but never in depth: however far it
//! shifts, the stack goes away from the viewer. The shape is
//! `recent-windows.flip`
//! (niri_config::MruFlip).

use niri_config::MruFlip;
use smithay::utils::{Logical, Point, Rectangle, Size};

/// Focal length, as a fraction of the output width.
const FOCAL: f64 = 1.1;
/// Where a window is in the scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// Centre of the window.
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Size relative to the window's own logical size.
    pub scale: f64,
    /// Turn to face right (positive) or left, in radians.
    pub yaw: f64,
    /// Then to face down (positive) or up, in radians.
    pub pitch: f64,
}

/// The window's axes in the scene for a turn: across it (left to right),
/// down it, and straight back from it.
pub fn axes(yaw: f64, pitch: f64) -> [(f64, f64, f64); 3] {
    let (sa, ca) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    [
        (ca, 0., sa),
        (-sp * sa, cp, sp * ca),
        (-cp * sa, -sp, cp * ca),
    ]
}

impl Pose {
    /// A window lying flat on the screen over `rect`, its on-screen rectangle.
    pub fn flat(rect: Rectangle<f64, Logical>, window: Size<f64, Logical>) -> Self {
        Self {
            x: rect.loc.x + rect.size.w / 2.,
            y: rect.loc.y + rect.size.h / 2.,
            z: 0.,
            scale: rect.size.w / window.w.max(1.),
            yaw: 0.,
            pitch: 0.,
        }
    }

    pub fn lerp(self, to: Self, t: f64) -> Self {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Self {
            x: mix(self.x, to.x),
            y: mix(self.y, to.y),
            z: mix(self.z, to.z),
            scale: mix(self.scale, to.scale),
            yaw: mix(self.yaw, to.yaw),
            pitch: mix(self.pitch, to.pitch),
        }
    }
}

/// The camera looking at the stack on one output.
#[derive(Debug, Clone, Copy)]
pub struct Stage {
    output: Size<f64, Logical>,
    shape: MruFlip,
}

impl Stage {
    pub fn new(output: Size<f64, Logical>, shape: MruFlip) -> Self {
        Self { output, shape }
    }

    fn focal(&self) -> f64 {
        FOCAL * self.output.w.max(1.)
    }

    /// The front window's centre.
    fn front(&self) -> Point<f64, Logical> {
        Point::new(
            self.shape.front_x / 100. * self.output.w,
            self.shape.front_y / 100. * self.output.h,
        )
    }

    /// Where the camera looks from, and the stack narrows to: the front
    /// window's centre, moved by the camera's offset.
    fn center(&self) -> Point<f64, Logical> {
        let front = self.front();
        Point::new(
            front.x + self.shape.camera_x / 100. * self.output.w,
            front.y + self.shape.camera_y / 100. * self.output.h,
        )
    }

    /// One slot further back: straight back from the windows by the
    /// spacing, and along them by the offset, flat on the screen (an offset
    /// in depth could bring the windows behind in front); across, down and
    /// into the screen.
    fn step(&self) -> (f64, f64, f64) {
        let (yaw, pitch) = self.turn();
        let [across, down, back] = axes(yaw, pitch);
        let w = self.output.w / 100.;
        let (d, ox, oy) = (
            self.shape.spacing * w,
            self.shape.offset_x * w,
            self.shape.offset_y * w,
        );
        (
            back.0 * d + across.0 * ox + down.0 * oy,
            back.1 * d + across.1 * ox + down.1 * oy,
            back.2 * d,
        )
    }

    fn turn(&self) -> (f64, f64) {
        (
            self.shape.tilt_x.to_radians(),
            self.shape.tilt_y.to_radians(),
        )
    }

    /// The pose of a window at `slot` in the stack: 0 is the front, 1 the one
    /// behind it, and so on. Fractional slots lie in between; slots below 0
    /// continue past the front towards the viewer.
    pub fn slot_pose(&self, slot: f64, window: Size<f64, Logical>) -> Pose {
        let (w, h) = (self.output.w, self.output.h);
        let card = self.shape.card_size / 100.;
        let scale = f64::min(
            1.,
            f64::min(card * w / window.w.max(1.), card * h / window.h.max(1.)),
        );
        let front = self.front();
        let (dx, dy, dz) = self.step();
        let (yaw, pitch) = self.turn();
        let [across, down, _] = axes(yaw, pitch);

        // Windows line up by their leading edge (the side the stack goes
        // towards), not their centre, so each shows as much of itself behind
        // the one in front whatever its size: one smaller than the largest
        // card is shifted by the difference. The lead fades out as the stack
        // goes straight back, where windows stay centred.
        let m = dx.abs().max(dy.abs()).max(0.02 * w);
        let (lead_u, lead_v) = (dx / m, dy / m);
        let shift_u = lead_u * (card * w - window.w * scale) / 2.;
        let shift_v = lead_v * (card * h - window.h * scale) / 2.;

        Pose {
            x: front.x + slot * dx + shift_u * across.0 + shift_v * down.0,
            y: front.y + slot * dy + shift_u * across.1 + shift_v * down.1,
            z: slot * dz,
            scale,
            yaw,
            pitch,
        }
    }

    /// Depth of a slot, for ordering cards front to back.
    pub fn slot_depth(&self, slot: f64) -> f64 {
        slot * self.step().2
    }

    fn project_point(&self, x: f64, y: f64, z: f64) -> Point<f64, Logical> {
        let f = self.focal();
        // Never at or behind the camera.
        let s = f / f64::max(f + z, 0.05 * f);
        let c = self.center();
        Point::new(c.x + (x - c.x) * s, c.y + (y - c.y) * s)
    }

    /// The window's corners on the output, clockwise from the top left (the
    /// corners of its rectangle `(0, 0)`–`window`).
    pub fn project(&self, pose: Pose, window: Size<f64, Logical>) -> [Point<f64, Logical>; 4] {
        let [across, down, _] = axes(pose.yaw, pose.pitch);
        let (hw, hh) = (window.w * pose.scale / 2., window.h * pose.scale / 2.);
        [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(u, v)| {
            self.project_point(
                pose.x + u * across.0 + v * down.0,
                pose.y + u * across.1 + v * down.1,
                pose.z + u * across.2 + v * down.2,
            )
        })
    }
}

/// Where window `index` of `count` sits in the stack while the selection is at
/// `scroll` (the selected window, at the front, has `index == scroll`).
///
/// Slots run from -0.5 to `count` - 0.5: a window scrolled off the front
/// carries on towards the viewer and, past -0.5, comes back in at the far end
/// of the stack, where `slot_alpha` fades it out and in.
pub fn slot_of(index: usize, scroll: f64, count: usize) -> f64 {
    if count == 0 {
        return 0.;
    }
    (index as f64 - scroll + 0.5).rem_euclid(count as f64) - 0.5
}

/// Opacity of a window at `slot` of a stack of `count` showing `shown` at
/// once: fading out as it leaves the front, in as it arrives at the back, and
/// out past the windows shown (the next one fades in and out as the stack
/// scrolls; deeper ones are not drawn).
pub fn slot_alpha(slot: f64, count: usize, shown: f64) -> f64 {
    let last = count.saturating_sub(1) as f64;
    let mut alpha: f64 = 1.;
    if slot < 0. {
        alpha = alpha.min(1. + 2. * slot);
    }
    if count > 1 && slot > last {
        alpha = alpha.min(1. - 2. * (slot - last));
    }
    alpha = alpha.min(shown.max(1.) - slot);
    alpha.clamp(0., 1.)
}

/// The shortest move from the selection position `scroll` to window `index`
/// of `count`: forwards or backwards around the stack.
pub fn shortest_delta(scroll: f64, index: usize, count: usize) -> f64 {
    if count == 0 {
        return 0.;
    }
    let n = count as f64;
    let delta = (index as f64 - scroll).rem_euclid(n);
    if delta > n / 2. {
        delta - n
    } else {
        delta
    }
}

/// A 3×3 matrix acting on homogeneous 2D points, rows first.
pub type Mat3 = [[f64; 3]; 3];

/// The projective map taking the rectangle `(0, 0)`–`size` onto `quad`
/// (corners clockwise from the top left).
pub fn homography(size: Size<f64, Logical>, quad: [Point<f64, Logical>; 4]) -> Mat3 {
    // Unit square to quad (Heckbert), then the rectangle onto the unit square.
    let [p0, p1, p2, p3] = quad;
    let sx = p0.x - p1.x + p2.x - p3.x;
    let sy = p0.y - p1.y + p2.y - p3.y;
    let (dx1, dx2) = (p1.x - p2.x, p3.x - p2.x);
    let (dy1, dy2) = (p1.y - p2.y, p3.y - p2.y);
    let det = dx1 * dy2 - dx2 * dy1;
    let (g, h) = if det.abs() < 1e-12 {
        (0., 0.)
    } else {
        ((sx * dy2 - dx2 * sy) / det, (dx1 * sy - sx * dy1) / det)
    };
    let square = [
        [p1.x - p0.x + g * p1.x, p3.x - p0.x + h * p3.x, p0.x],
        [p1.y - p0.y + g * p1.y, p3.y - p0.y + h * p3.y, p0.y],
        [g, h, 1.],
    ];
    let unit = [
        [1. / size.w.max(1e-9), 0., 0.],
        [0., 1. / size.h.max(1e-9), 0.],
        [0., 0., 1.],
    ];
    mul(square, unit)
}

pub fn mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut m = [[0.; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

pub fn invert(m: Mat3) -> Option<Mat3> {
    let [[a, b, c], [d, e, f], [g, h, i]] = m;
    let co = [
        [e * i - f * h, c * h - b * i, b * f - c * e],
        [f * g - d * i, a * i - c * g, c * d - a * f],
        [d * h - e * g, b * g - a * h, a * e - b * d],
    ];
    let det = a * co[0][0] + b * co[1][0] + c * co[2][0];
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    Some(co.map(|row| row.map(|x| x / det)))
}

/// `m` applied to a point, with the perspective divide.
#[cfg(test)]
pub fn apply(m: Mat3, p: Point<f64, Logical>) -> Point<f64, Logical> {
    let x = m[0][0] * p.x + m[0][1] * p.y + m[0][2];
    let y = m[1][0] * p.x + m[1][1] * p.y + m[1][2];
    let w = m[2][0] * p.x + m[2][1] * p.y + m[2][2];
    Point::new(x / w, y / w)
}

/// Whether `p` is inside the convex `quad`.
pub fn quad_contains(quad: &[Point<f64, Logical>; 4], p: Point<f64, Logical>) -> bool {
    let mut sign = 0.;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross.abs() < 1e-9 {
            continue;
        }
        if sign == 0. {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// The axis-aligned box around `quad`.
pub fn bounds(quad: &[Point<f64, Logical>; 4]) -> Rectangle<f64, Logical> {
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in quad {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Rectangle::new(Point::new(x0, y0), Size::new(x1 - x0, y1 - y0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point<f64, Logical>, b: Point<f64, Logical>) -> bool {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
    }

    /// The shape the geometry tests were written for (before the default
    /// became Vista's): looked at straight on, facing right and down.
    fn classic() -> MruFlip {
        MruFlip {
            tilt_x: 24.,
            tilt_y: 10.,
            spacing: 27.,
            offset_x: 0.,
            offset_y: 0.,
            front_x: 60.,
            front_y: 54.,
            camera_x: 0.,
            camera_y: 0.,
            card_size: 50.,
            visible_windows: 10.,
        }
    }

    #[test]
    fn the_default_is_vistas() {
        assert_eq!(
            MruFlip::default(),
            MruFlip {
                tilt_x: 23.,
                tilt_y: 2.,
                spacing: 44.,
                front_x: 69.,
                front_y: 60.,
                camera_x: -15.,
                camera_y: -32.,
                card_size: 43.,
                ..classic()
            }
        );
    }

    fn output() -> Size<f64, Logical> {
        Size::new(1512., 945.)
    }

    #[test]
    fn a_flat_pose_projects_onto_its_rectangle() {
        let stage = Stage::new(output(), classic());
        let window = Size::new(800., 600.);
        let rect = Rectangle::new(Point::new(100., 50.), window);
        let quad = stage.project(Pose::flat(rect, window), window);
        assert!(close(quad[0], Point::new(100., 50.)));
        assert!(close(quad[1], Point::new(900., 50.)));
        assert!(close(quad[2], Point::new(900., 650.)));
        assert!(close(quad[3], Point::new(100., 650.)));
    }

    #[test]
    fn the_stack_recedes_up_and_left_with_the_left_edges_nearer() {
        let stage = Stage::new(output(), classic());
        let window = Size::new(1200., 800.);
        let front = stage.project(stage.slot_pose(0., window), window);
        let back = stage.project(stage.slot_pose(1., window), window);
        // Further back: smaller, up and to the left.
        assert!(back[0].x < front[0].x && back[0].y < front[0].y);
        assert!(bounds(&back).size.w < bounds(&front).size.w);
        // The left edge, nearer, is taller than the right.
        let left = front[3].y - front[0].y;
        let right = front[2].y - front[1].y;
        assert!(left > right);
        // The front card fits on the output.
        let b = bounds(&front);
        assert!(b.loc.x >= 0. && b.loc.y >= 0.);
        assert!(b.loc.x + b.size.w <= output().w && b.loc.y + b.size.h <= output().h);
        // The default shape puts the next window up and to the left.
        let next = stage.project(stage.slot_pose(1., window), window);
        assert!(bounds(&next).loc.x < bounds(&front).loc.x);
        // Small windows keep their size, large ones are capped.
        assert_eq!(stage.slot_pose(0., Size::new(300., 200.)).scale, 1.);
        assert!(stage.slot_pose(0., window).scale < 1.);
    }

    /// The same corners as the Settings app's preview
    /// (~/code/alniac/apps/settings/tests/test_flip_model.js): the two must
    /// draw the same stack.
    #[test]
    fn classic_corners_match_the_settings_preview() {
        let stage = Stage::new(output(), classic());
        let window = Size::new(1200., 800.);
        let expected = [
            [
                (538.281, 249.119),
                (1206.826, 291.66),
                (1164.566, 720.056),
                (520.388, 758.904),
            ],
            [
                (464.465, 237.232),
                (1028.074, 274.066),
                (998.887, 631.93),
                (452.864, 650.156),
            ],
        ];
        for (slot, corners) in expected.iter().enumerate() {
            let quad = stage.project(stage.slot_pose(slot as f64, window), window);
            for (p, (x, y)) in quad.iter().zip(corners) {
                assert!(
                    (p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3,
                    "{slot}: {p:?}"
                );
            }
        }
    }

    #[test]
    fn the_stack_goes_back_opposite_to_where_the_windows_face() {
        let window = Size::new(1200., 800.);
        let centre = |stage: &Stage, slot: f64| {
            let b = bounds(&stage.project(stage.slot_pose(slot, window), window));
            Point::new(b.loc.x + b.size.w / 2., b.loc.y + b.size.h / 2.)
        };
        let shape = |tilt_x, tilt_y| {
            Stage::new(
                output(),
                MruFlip {
                    tilt_x,
                    tilt_y,
                    ..classic()
                },
            )
        };

        // Facing you: straight back, every window centred behind the front one.
        let straight = shape(0., 0.);
        let front = centre(&straight, 0.);
        assert!(close(front, Point::new(0.6 * 1512., 0.54 * 945.)));
        for slot in [1., 2., 5.] {
            assert!(close(centre(&straight, slot), front));
        }
        let size = |stage: &Stage, slot| {
            bounds(&stage.project(stage.slot_pose(slot, window), window))
                .size
                .w
        };
        assert!(size(&straight, 1.) < size(&straight, 0.));

        // Facing right and down: the stack recedes left and up; the left and
        // top edges come nearer (the longer ones).
        let right_down = shape(24., 10.);
        let (a, b) = (centre(&right_down, 0.), centre(&right_down, 1.));
        assert!(b.x < a.x && b.y < a.y);
        let q = right_down.project(right_down.slot_pose(0., window), window);
        assert!(q[3].y - q[0].y > q[2].y - q[1].y, "left edge nearer");
        assert!(q[1].x - q[0].x > q[2].x - q[3].x, "top edge nearer");

        // Facing left and up: right and down.
        let left_up = shape(-24., -10.);
        let (a, b) = (centre(&left_up, 0.), centre(&left_up, 1.));
        assert!(b.x > a.x && b.y > a.y);

        // Facing only down: straight up, not sideways.
        let down = shape(0., 20.);
        let (a, b) = (centre(&down, 0.), centre(&down, 1.));
        assert!((b.x - a.x).abs() < 1e-6 && b.y < a.y);
    }

    #[test]
    fn the_offset_shifts_each_window_along_the_windows() {
        let window = Size::new(1200., 800.);
        let centre = |stage: &Stage, slot: f64| {
            let q = stage.project(stage.slot_pose(slot, window), window);
            Point::<f64, Logical>::new(
                q.iter().map(|p| p.x).sum::<f64>() / 4.,
                q.iter().map(|p| p.y).sum::<f64>() / 4.,
            )
        };
        // Facing you, offset left: each window peeks out to the left, level.
        let fan = Stage::new(
            output(),
            MruFlip {
                tilt_x: 0.,
                tilt_y: 0.,
                offset_x: -8.,
                ..classic()
            },
        );
        let (a, b) = (centre(&fan, 0.), centre(&fan, 1.));
        assert!(b.x < a.x && (b.y - a.y).abs() < 1e-6);
        // Pinned, shared with Settings' preview.
        let stage = Stage::new(
            output(),
            MruFlip {
                offset_x: -8.,
                offset_y: -4.,
                ..classic()
            },
        );
        let q = stage.project(stage.slot_pose(1., window), window);
        let expected = [
            (368.902, 183.652),
            (945.401, 227.713),
            (919.015, 587.148),
            (361.024, 598.663),
        ];
        for (p, (x, y)) in q.iter().zip(expected) {
            assert!((p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3, "{p:?}");
        }
        // However far the offset goes, and whichever way the windows face,
        // each window behind is further away, never in front.
        for (tilt_x, offset_x) in [(-40., 30.), (40., -30.), (60., 30.), (-60., -30.)] {
            let stage = Stage::new(
                output(),
                MruFlip {
                    tilt_x,
                    tilt_y: -10.,
                    offset_x,
                    spacing: 10.,
                    ..classic()
                },
            );
            let depths: Vec<f64> = (0..4)
                .map(|k| stage.slot_pose(k as f64, window).z)
                .collect();
            assert!(
                depths.windows(2).all(|w| w[1] > w[0]),
                "{tilt_x} {offset_x}: {depths:?}"
            );
        }
    }

    #[test]
    fn the_camera_and_card_size_shape_the_view() {
        let window = Size::new(1200., 800.);
        // Vista's Flip 3D, looked at from above (shared with Settings'
        // preview): the front window's top edge almost level, its bottom
        // rising to the right.
        let vista = Stage::new(
            output(),
            MruFlip {
                tilt_x: 23.,
                tilt_y: 2.,
                spacing: 44.,
                front_x: 69.,
                front_y: 60.,
                camera_x: -15.,
                camera_y: -32.,
                card_size: 43.,
                ..classic()
            },
        );
        let expected = [
            [
                (741.122, 372.065),
                (1276.737, 357.652),
                (1268.226, 734.559),
                (735.813, 806.739),
            ],
            [
                (561.574, 323.504),
                (977.812, 317.629),
                (973.096, 598.672),
                (558.852, 635.462),
            ],
        ];
        for (slot, corners) in expected.iter().enumerate() {
            let quad = vista.project(vista.slot_pose(slot as f64, window), window);
            for (p, (x, y)) in quad.iter().zip(corners) {
                assert!(
                    (p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3,
                    "{slot}: {p:?}"
                );
            }
        }
        let q = vista.project(vista.slot_pose(0., window), window);
        assert!((q[1].y - q[0].y).abs() < 20. && q[3].y - q[2].y > 60.);
        // A window lying flat still lands on itself wherever the camera is.
        let rect = Rectangle::new(Point::new(100., 50.), window);
        let flat = vista.project(Pose::flat(rect, window), window);
        assert!(close(flat[0], Point::new(100., 50.)) && close(flat[2], Point::new(1300., 850.)));
        // Card size caps a large window.
        let small = Stage::new(
            output(),
            MruFlip {
                card_size: 25.,
                ..classic()
            },
        );
        assert!((small.slot_pose(0., window).scale - 0.25 * 945. / 800.).abs() < 1e-9);
    }

    #[test]
    fn windows_of_any_size_show_even_strips() {
        // Fan: facing you, each window shifted left. Windows of different
        // sizes line up by their left edges, so the strips step evenly
        // (closer further back), never a window hidden behind a wider one.
        let stage = Stage::new(
            output(),
            MruFlip {
                tilt_x: 0.,
                tilt_y: 0.,
                offset_x: -10.,
                spacing: 14.,
                front_x: 62.,
                front_y: 50.,
                ..classic()
            },
        );
        let sizes = [
            (0.95, 0.93),
            (0.62, 0.93),
            (0.95, 0.93),
            (0.48, 0.6),
            (0.8, 0.85),
            (0.5, 0.93),
        ];
        let lefts: Vec<f64> = sizes
            .iter()
            .enumerate()
            .map(|(k, (w, h))| {
                let window = Size::new(w * 1512., h * 945.);
                let q = stage.project(stage.slot_pose(k as f64, window), window);
                q.iter().map(|p| p.x).fold(f64::INFINITY, f64::min)
            })
            .collect();
        let steps: Vec<f64> = lefts.windows(2).map(|w| w[0] - w[1]).collect();
        assert!(steps.iter().all(|s| *s > 0.), "{lefts:?}");
        assert!(steps.windows(2).all(|w| w[1] <= w[0] + 1e-6), "{steps:?}");
        // Straight back, they stay centred.
        let straight = Stage::new(
            output(),
            MruFlip {
                tilt_x: 0.,
                tilt_y: 0.,
                ..classic()
            },
        );
        let small = Size::new(400., 300.);
        let pose = straight.slot_pose(2., small);
        assert!((pose.x - 0.6 * 1512.).abs() < 1e-9 && (pose.y - 0.54 * 945.).abs() < 1e-9);
    }

    #[test]
    fn the_axes_stay_square() {
        let dot = |a: (f64, f64, f64), b: (f64, f64, f64)| a.0 * b.0 + a.1 * b.1 + a.2 * b.2;
        for (yaw, pitch) in [(0., 0.), (0.4, 0.2), (-1., 0.7)] {
            let [x, y, z] = axes(yaw, pitch);
            assert!(dot(x, y).abs() < 1e-12 && dot(y, z).abs() < 1e-12 && dot(x, z).abs() < 1e-12);
            for v in [x, y, z] {
                assert!((dot(v, v) - 1.).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn the_homography_maps_the_window_onto_its_quad() {
        let stage = Stage::new(output(), classic());
        let window = Size::new(1000., 700.);
        for slot in [-0.4, 0., 0.5, 3.] {
            let quad = stage.project(stage.slot_pose(slot, window), window);
            let h = homography(window, quad);
            let corners = [
                Point::new(0., 0.),
                Point::new(1000., 0.),
                Point::new(1000., 700.),
                Point::new(0., 700.),
            ];
            for (corner, expected) in corners.into_iter().zip(quad) {
                assert!(close(apply(h, corner), expected), "{slot}");
            }
            // And back.
            let inv = invert(h).unwrap();
            for (corner, projected) in corners.into_iter().zip(quad) {
                assert!(close(apply(inv, projected), corner));
            }
            // The window's centre lands where the scene puts it: on the
            // projection of the centre, not the quad's diagonal midpoint
            // average, which perspective would skew.
            let pose = stage.slot_pose(slot, window);
            let centre = stage.project_point(pose.x, pose.y, pose.z);
            assert!(close(apply(h, Point::new(500., 350.)), centre));
        }
        // An affine quad is an affine map.
        let rect = Rectangle::new(Point::new(10., 20.), Size::new(500., 350.));
        let quad = stage.project(Pose::flat(rect, window), window);
        let h = homography(window, quad);
        assert!(h[2][0].abs() < 1e-12 && h[2][1].abs() < 1e-12);
        assert!(close(
            apply(h, Point::new(500., 350.)),
            Point::new(260., 195.)
        ));
    }

    #[test]
    fn slots_wrap_through_the_back_of_the_stack() {
        // Five windows, the third selected: it is at the front, the ones
        // after it behind, the ones before it at the back.
        let slots: Vec<f64> = (0..5).map(|i| slot_of(i, 2., 5)).collect();
        assert_eq!(slots, [3., 4., 0., 1., 2.]);
        // Half way to the next window: the old front is leaving, faded out
        // at -0.5, and every other window has moved half a slot forward.
        let slots: Vec<f64> = (0..5).map(|i| slot_of(i, 2.5, 5)).collect();
        assert_eq!(slots, [2.5, 3.5, -0.5, 0.5, 1.5]);
        assert_eq!(slot_alpha(-0.5, 5, 10.), 0.);
        // Just past that, it is back in, faded out, at the end of the stack.
        let slot = slot_of(2, 2.51, 5);
        assert!((slot - 4.49).abs() < 1e-9);
        assert!(slot_alpha(slot, 5, 10.) < 0.03);
        assert_eq!(slot_alpha(4., 5, 10.), 1.);
        assert_eq!(slot_alpha(0., 5, 10.), 1.);
        // Scrolling far still wraps.
        assert_eq!(slot_of(0, 7., 5), 3.);
        assert_eq!(slot_of(0, -3., 5), 3.);
        // One window stays at the front.
        assert_eq!(slot_of(0, 0.3, 1), -0.3);
        assert_eq!(slot_of(0, 0., 1), 0.);
        // Deep stacks show only so many: 10 shown is slots 0 to 9, the next
        // fading out as the stack scrolls.
        assert_eq!(slot_alpha(9., 30, 10.), 1.);
        assert_eq!(slot_alpha(9.5, 30, 10.), 0.5);
        assert_eq!(slot_alpha(10., 30, 10.), 0.);
        assert_eq!(slot_alpha(2., 30, 3.), 1.);
        assert_eq!(slot_alpha(3., 30, 3.), 0.);
        // Fewer windows than shown: all of them.
        assert_eq!(slot_alpha(4., 5, 10.), 1.);
    }

    #[test]
    fn the_selection_takes_the_short_way_round() {
        assert_eq!(shortest_delta(2., 3, 5), 1.);
        assert_eq!(shortest_delta(2., 1, 5), -1.);
        // From the last to the first is one step forward.
        assert_eq!(shortest_delta(4., 0, 5), 1.);
        assert_eq!(shortest_delta(0., 4, 5), -1.);
        // An unbounded position counts modulo the stack.
        assert_eq!(shortest_delta(12., 3, 5), 1.);
        assert_eq!(shortest_delta(-1., 0, 5), 1.);
        assert_eq!(shortest_delta(2., 2, 5), 0.);
        assert_eq!(shortest_delta(0., 0, 0), 0.);
    }

    #[test]
    fn hit_testing_and_bounds() {
        let quad = [
            Point::new(0., 0.),
            Point::new(10., 2.),
            Point::new(10., 8.),
            Point::new(0., 10.),
        ];
        assert!(quad_contains(&quad, Point::new(5., 5.)));
        assert!(!quad_contains(&quad, Point::new(9., 1.)));
        assert!(!quad_contains(&quad, Point::new(-1., 5.)));
        let b = bounds(&quad);
        assert_eq!(b, Rectangle::new(Point::new(0., 0.), Size::new(10., 10.)));
    }

    #[test]
    fn poses_interpolate_from_flat_to_the_stack() {
        let stage = Stage::new(output(), classic());
        let window = Size::new(800., 600.);
        let flat = Pose::flat(Rectangle::new(Point::new(0., 0.), window), window);
        let slot = stage.slot_pose(0., window);
        assert_eq!(flat.lerp(slot, 0.), flat);
        assert_eq!(flat.lerp(slot, 1.), slot);
        let mid = flat.lerp(slot, 0.5);
        assert!((mid.yaw - 12_f64.to_radians()).abs() < 1e-12);
        assert!((mid.pitch - 5_f64.to_radians()).abs() < 1e-12);
    }
}
