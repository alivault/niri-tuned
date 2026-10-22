//! Compiz-style wobbly windows: the spring model.
//!
//! As in Compiz's wobbly plugin (the spring model by Kristian Høgsberg), a window is a 4 × 4
//! grid of masses joined to their neighbours by springs. A grabbed mass follows the window
//! exactly; the others lag behind it and swing back. The 16 masses are the control points of a
//! bicubic Bézier patch, which the renderer bends the window's picture onto.
//!
//! Positions are displacements from each mass's place on the undeformed window, in logical
//! pixels, so the model does not care about the window's size or where it is. A spring pulls on
//! the difference of its ends' displacements: the same force as Compiz's `b - a - offset`.
//!
//! Compiz steps the physics every 15 ms. Here a 15 ms step is the unit of time too, so friction
//! and spring constants mean the same as in Compiz, but each frame is integrated in short
//! sub-steps over the time that passed on the compositor clock: the wobble is the same at 60 and
//! 120 Hz, and follows the animation slowdown.

use std::time::Duration;

use glam::DVec2;

/// Masses along each side.
pub const GRID: usize = 4;
const NODES: usize = GRID * GRID;
/// Compiz's mass of each point.
const MASS: f64 = 15.;
/// Compiz's step, in milliseconds: the unit of time of the physics.
const STEP_MS: f64 = 15.;
/// Longest sub-step, in steps.
const MAX_SUBSTEP: f64 = 0.25;
/// Longest time one frame advances, in milliseconds (a frame that comes late).
const MAX_FRAME_MS: f64 = 50.;
/// Below these (pixels, and pixels per step) the window is at rest.
const REST_DISTANCE: f64 = 0.25;
const REST_VELOCITY: f64 = 0.05;
/// The four inner masses: they hold a window that moves without being grabbed.
const MIDDLE: [usize; 4] = [GRID + 1, GRID + 2, 2 * GRID + 1, 2 * GRID + 2];

#[derive(Debug, Clone)]
pub struct WobblyModel {
    disp: [DVec2; NODES],
    vel: [DVec2; NODES],
    /// Masses that follow the window exactly (Compiz's immobile anchor).
    anchored: [bool; NODES],
    grabbed: bool,
    /// Compositor time of the last step.
    time: Duration,
    /// Still and undeformed.
    resting: bool,
    /// It was at rest when something moved it: the next step starts the clock again, as the
    /// time at rest was not spent wobbling.
    stale_clock: bool,
}

impl WobblyModel {
    pub fn new(now: Duration) -> Self {
        Self {
            disp: [DVec2::ZERO; NODES],
            vel: [DVec2::ZERO; NODES],
            anchored: [false; NODES],
            grabbed: false,
            time: now,
            resting: true,
            stale_clock: true,
        }
    }

    /// Grabs the window at `at`, a point on it from 0 to 1: the nearest mass follows the pointer.
    ///
    /// Like Compiz, the grab pinches the springs around that mass a little.
    pub fn grab(&mut self, at: DVec2, size: DVec2) {
        let pos = (at.clamp(DVec2::ZERO, DVec2::ONE) * (GRID - 1) as f64).round();
        let anchor = pos.y as usize * GRID + pos.x as usize;

        self.anchored = [false; NODES];
        self.anchored[anchor] = true;
        self.disp[anchor] = DVec2::ZERO;
        self.vel[anchor] = DVec2::ZERO;
        self.grabbed = true;

        let pad = size / (GRID - 1) as f64;
        for (a, b, offset) in springs(pad) {
            if a == anchor {
                self.vel[b] -= offset * 0.05;
            } else if b == anchor {
                self.vel[a] += offset * 0.05;
            }
        }
        self.wake();
    }

    fn wake(&mut self) {
        if self.resting {
            self.resting = false;
            self.stale_clock = true;
        }
    }

    /// Lets go: the grabbed mass still holds the window until it comes to rest.
    pub fn release(&mut self) {
        self.grabbed = false;
    }

    pub fn is_grabbed(&self) -> bool {
        self.grabbed
    }

    /// The window moved by `delta` on screen: the free masses stay behind.
    pub fn translate(&mut self, delta: DVec2) {
        if delta == DVec2::ZERO {
            return;
        }
        if !self.anchored.contains(&true) {
            for i in MIDDLE {
                self.anchored[i] = true;
            }
        }
        for i in 0..NODES {
            if !self.anchored[i] {
                self.disp[i] -= delta;
            }
        }
        self.wake();
    }

    /// Compiz's map effect: the edges spring out from the middle, by `random()` (0 to 1) each.
    pub fn shiver(&mut self, size: DVec2, mut random: impl FnMut() -> f64) {
        if !self.anchored.contains(&true) {
            for i in MIDDLE {
                self.anchored[i] = true;
            }
        }
        let center = size / 2.;
        for i in 0..NODES {
            if self.anchored[i] {
                continue;
            }
            let rest = rest_position(i, size);
            let dir = (rest + self.disp[i] - center) / size.max(DVec2::ONE);
            self.vel[i] += dir * (random() * 7.5);
        }
        self.wake();
    }

    /// Steps the physics up to `now`.
    pub fn advance(&mut self, now: Duration, friction: f64, spring_k: f64) {
        if self.resting || self.stale_clock {
            self.time = now;
            self.stale_clock = false;
            return;
        }

        let ms = now.saturating_sub(self.time).as_secs_f64() * 1000.;
        self.time = now;
        let steps = ms.min(MAX_FRAME_MS) / STEP_MS;
        if steps <= 0. {
            return;
        }

        let n = (steps / MAX_SUBSTEP).ceil().max(1.);
        let h = steps / n;
        for _ in 0..n as usize {
            self.substep(h, friction, spring_k);
        }

        if self.is_still() {
            self.disp = [DVec2::ZERO; NODES];
            self.vel = [DVec2::ZERO; NODES];
            self.resting = true;
        }
    }

    fn substep(&mut self, h: f64, friction: f64, spring_k: f64) {
        let mut force = [DVec2::ZERO; NODES];
        // Spring offsets cancel out in displacements.
        for (a, b, _) in springs(DVec2::ZERO) {
            let d = 0.5 * spring_k * (self.disp[b] - self.disp[a]);
            force[a] += d;
            force[b] -= d;
        }
        let nodes = self.disp.iter_mut().zip(&mut self.vel).zip(&self.anchored);
        for (((disp, vel), anchored), force) in nodes.zip(force) {
            if *anchored {
                *vel = DVec2::ZERO;
                continue;
            }
            *vel += (force - friction * *vel) / MASS * h;
            *disp += *vel * h;
        }
    }

    fn is_still(&self) -> bool {
        self.disp
            .iter()
            .all(|d| d.abs().max_element() < REST_DISTANCE)
            && self
                .vel
                .iter()
                .all(|v| v.abs().max_element() < REST_VELOCITY)
    }

    /// Still and undeformed: nothing to draw differently, no frame to ask for.
    pub fn is_at_rest(&self) -> bool {
        self.resting
    }

    /// The patch's control points on a window of `size`, rows first.
    pub fn control_points(&self, size: DVec2) -> [DVec2; NODES] {
        std::array::from_fn(|i| rest_position(i, size) + self.disp[i])
    }

    #[cfg(test)]
    fn displacement(&self, x: usize, y: usize) -> DVec2 {
        self.disp[y * GRID + x]
    }
}

fn rest_position(i: usize, size: DVec2) -> DVec2 {
    let (x, y) = (i % GRID, i / GRID);
    DVec2::new(x as f64, y as f64) / (GRID - 1) as f64 * size
}

/// The springs between neighbours: (a, b, b's rest place relative to a's, with `pad` between
/// masses).
fn springs(pad: DVec2) -> impl Iterator<Item = (usize, usize, DVec2)> {
    (0..NODES).flat_map(move |i| {
        let (x, y) = (i % GRID, i / GRID);
        let left = (x > 0).then(|| (i - 1, i, DVec2::new(pad.x, 0.)));
        let up = (y > 0).then(|| (i - GRID, i, DVec2::new(0., pad.y)));
        left.into_iter().chain(up)
    })
}

/// The point of the bicubic Bézier patch over `points` at (u, v); outside 0..1 it goes on
/// smoothly (a window's shadow is outside its patch).
pub fn evaluate(points: &[DVec2; NODES], u: f64, v: f64) -> DVec2 {
    let bu = bernstein(u);
    let bv = bernstein(v);
    let mut p = DVec2::ZERO;
    for (j, bv) in bv.iter().enumerate() {
        for (i, bu) in bu.iter().enumerate() {
            p += points[j * GRID + i] * (bu * bv);
        }
    }
    p
}

fn bernstein(t: f64) -> [f64; 4] {
    let s = 1. - t;
    [s * s * s, 3. * t * s * s, 3. * t * t * s, t * t * t]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRICTION: f64 = 3.;
    const SPRING_K: f64 = 8.;
    const SIZE: DVec2 = DVec2::new(800., 600.);

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// Runs frames `frame` ms apart until `until` ms.
    fn run(model: &mut WobblyModel, from: u64, until: u64, frame: u64) {
        let mut t = from;
        while t < until {
            t += frame;
            model.advance(ms(t), FRICTION, SPRING_K);
        }
    }

    #[test]
    fn rest_patch_is_the_window() {
        let model = WobblyModel::new(ms(0));
        let points = model.control_points(SIZE);
        for (u, v) in [(0., 0.), (1., 1.), (0.25, 0.75), (0.5, 0.1)] {
            let p = evaluate(&points, u, v);
            assert!((p - DVec2::new(u, v) * SIZE).length() < 1e-9);
        }
        // Outside the patch too (the shadow).
        let p = evaluate(&points, -0.1, 1.2);
        assert!((p - DVec2::new(-80., 720.)).length() < 1e-9);
    }

    #[test]
    fn drag_leads_and_far_corner_lags() {
        let mut model = WobblyModel::new(ms(0));
        // Grab the top-left corner and drag right.
        model.grab(DVec2::ZERO, SIZE);
        model.advance(ms(0), FRICTION, SPRING_K);
        for t in 1..=10 {
            model.translate(DVec2::new(20., 0.));
            model.advance(ms(t * 8), FRICTION, SPRING_K);
        }

        assert_eq!(
            model.displacement(0, 0),
            DVec2::ZERO,
            "the grabbed corner follows"
        );
        let far = model.displacement(3, 3);
        let near = model.displacement(1, 0);
        assert!(far.x < -20., "the far corner lags: {far:?}");
        assert!(far.x < near.x, "the far corner lags more than a neighbour");
        assert!(!model.is_at_rest());
    }

    #[test]
    fn settles_after_release() {
        let mut model = WobblyModel::new(ms(0));
        model.grab(DVec2::new(0.5, 0.), SIZE);
        model.advance(ms(0), FRICTION, SPRING_K);
        model.translate(DVec2::new(300., 100.));
        model.release();
        assert!(!model.is_grabbed());

        // It swings past its place (a wobble, not a slide) ...
        let mut overshot = false;
        let mut t = 0;
        while t < 1000 {
            t += 8;
            model.advance(ms(t), FRICTION, SPRING_K);
            overshot |= model.displacement(3, 3).x > 1.;
        }
        assert!(overshot);

        // ... and comes to rest, exactly in place.
        run(&mut model, t, 5000, 8);
        assert!(model.is_at_rest());
        assert!(model
            .control_points(SIZE)
            .iter()
            .enumerate()
            .all(|(i, p)| *p == rest_position(i, SIZE)));
    }

    #[test]
    fn more_friction_settles_sooner() {
        let time_to_rest = |friction: f64| {
            let mut model = WobblyModel::new(ms(0));
            model.grab(DVec2::ZERO, SIZE);
            model.advance(ms(0), friction, SPRING_K);
            model.translate(DVec2::new(200., 0.));
            let mut t = 0;
            while !model.is_at_rest() && t < 20_000 {
                t += 8;
                model.advance(ms(t), friction, SPRING_K);
            }
            t
        };
        let (slow, fast) = (time_to_rest(1.), time_to_rest(6.));
        assert!(
            fast < slow,
            "friction 6 rests at {fast} ms, friction 1 at {slow} ms"
        );
        assert!(slow < 20_000);
    }

    #[test]
    fn same_wobble_at_any_frame_rate() {
        let at = |frame: u64| {
            let mut model = WobblyModel::new(ms(0));
            model.grab(DVec2::ZERO, SIZE);
            model.advance(ms(0), FRICTION, SPRING_K);
            model.translate(DVec2::new(100., 0.));
            // A first frame right away, as the compositor draws the move.
            model.advance(ms(0), FRICTION, SPRING_K);
            run(&mut model, 0, 240, frame);
            model.displacement(3, 3)
        };
        let (a, b) = (at(8), at(16));
        assert!((a - b).length() < 1., "120 Hz {a:?} vs 60 Hz {b:?}");
    }

    #[test]
    fn time_stands_still_with_the_clock() {
        let mut model = WobblyModel::new(ms(0));
        model.grab(DVec2::ZERO, SIZE);
        model.advance(ms(0), FRICTION, SPRING_K);
        model.translate(DVec2::new(100., 0.));
        model.advance(ms(100), FRICTION, SPRING_K);
        let before = model.displacement(3, 3);
        model.advance(ms(100), FRICTION, SPRING_K);
        assert_eq!(model.displacement(3, 3), before);
    }

    #[test]
    fn a_long_gap_is_not_one_huge_step() {
        // Grabbed and held still, then moved after a pause: the pause is not physics time.
        let mut model = WobblyModel::new(ms(0));
        model.grab(DVec2::ZERO, SIZE);
        run(&mut model, 0, 5000, 8);
        assert!(model.is_at_rest());
        model.translate(DVec2::new(50., 0.));
        model.advance(ms(60_000), FRICTION, SPRING_K);
        assert!(model.displacement(3, 3).x < -49.);
    }

    #[test]
    fn grab_picks_the_nearest_mass() {
        let mut model = WobblyModel::new(ms(0));
        model.grab(DVec2::new(0.9, 0.4), SIZE);
        model.advance(ms(0), FRICTION, SPRING_K);
        model.translate(DVec2::new(0., 30.));
        assert_eq!(model.displacement(3, 1), DVec2::ZERO);
        assert_eq!(model.displacement(0, 0), DVec2::new(0., -30.));
    }

    #[test]
    fn a_move_without_grab_holds_the_middle() {
        let mut model = WobblyModel::new(ms(0));
        model.translate(DVec2::new(40., 0.));
        assert_eq!(model.displacement(1, 1), DVec2::ZERO);
        assert_eq!(model.displacement(2, 2), DVec2::ZERO);
        assert_eq!(model.displacement(0, 0), DVec2::new(-40., 0.));
        run(&mut model, 0, 5000, 8);
        assert!(model.is_at_rest());
    }

    #[test]
    fn shiver_moves_only_the_edges_and_settles() {
        let mut model = WobblyModel::new(ms(0));
        model.shiver(SIZE, || 1.);
        model.advance(ms(0), FRICTION, SPRING_K);
        run(&mut model, 0, 30, 8);
        // The top-left corner springs out up and left.
        let corner = model.displacement(0, 0);
        assert!(corner.x < 0. && corner.y < 0., "{corner:?}");
        assert_eq!(model.displacement(1, 1), DVec2::ZERO);
        run(&mut model, 32, 5000, 8);
        assert!(model.is_at_rest());
    }
}
