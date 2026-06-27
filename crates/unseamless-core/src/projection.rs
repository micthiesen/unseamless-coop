//! World→screen projection math — pure, **host-tested**, no game/SDK/OS deps.
//!
//! This is the math behind the overhead peer nameplates (see `docs/OVERLAY-RENDERING.md` >
//! "(Later) Overhead nameplates"): given the camera and a peer's world position, where on screen
//! does a label go, and should it be drawn at all? Keeping it here means the tricky part — the
//! perspective transform and the culling — is verified on the host (`scripts/test-core.sh`); the
//! cdylib just feeds it the live camera (the SDK's `cs/camera.rs` `CSCam` named fields) and consumes
//! the result.
//!
//! ## Why NDC, not pixels
//! [`Camera::project`] returns **normalized device coordinates** (NDC: `x,y ∈ [-1, 1]`, `+x` right,
//! `+y` up), not pixels. The game-thread feature does the projection (it reads game state), but the
//! actual framebuffer size is known on the **Present** thread (imgui's `display_size`). Splitting at
//! NDC keeps the resolution-dependent step ([`ndc_to_screen`]) on the side that has the resolution,
//! and the resolution-*independent* step (this module's heart) host-testable with no notion of pixels.
//!
//! ## Conventions assumed (rig-confirm; see the feature's TODOs)
//! The camera basis (`right`/`up`/`forward`) is taken as an **orthonormal** frame in world space, with
//! `forward` pointing **the way the camera looks** (so a point in front has positive view depth), and
//! `fov_y` the **vertical** field of view. These match the usual `CSCam` interpretation but the exact
//! handedness / fov axis is a runtime fact — if nameplates land mirrored or vertically squashed on the
//! rig, that's the knob to flip, not a bug in this math.

/// 3-component vector, plain `[f32; 3]` under the hood. Local helpers (dot/sub) keep the projection
/// readable without pulling in a linear-algebra crate (core stays dependency-light).
type Vec3 = [f32; 3];

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A pinhole camera: an orthonormal world-space frame plus the perspective parameters. Built in the
/// cdylib from the SDK `CSCam`'s named fields (`matrix` rows for the frame, `fov`/`aspect_ratio`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Camera world position (the `matrix` translation row / `CSCam::position`).
    pub position: Vec3,
    /// Unit "right" axis (`matrix` row 0 / `CSCamExt::right`).
    pub right: Vec3,
    /// Unit "up" axis (`matrix` row 1 / `CSCamExt::up`).
    pub up: Vec3,
    /// Unit "forward" axis, pointing where the camera looks (`matrix` row 2 / `CSCamExt::forward`).
    pub forward: Vec3,
    /// Vertical field of view, in **radians**.
    pub fov_y: f32,
    /// Viewport aspect ratio, `width / height`.
    pub aspect_ratio: f32,
}

/// A world point successfully projected in front of the camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projected {
    /// Normalized device coordinates: `x,y ∈ [-1, 1]` when on screen, `+x` right, `+y` up. Values
    /// outside that range are in front of the camera but off the edges of the frame (see
    /// [`Projected::on_screen`]).
    pub ndc: [f32; 2],
    /// View-space depth: distance from the camera along `forward`, in meters. Always `> 0` (a point
    /// at or behind the camera plane yields `None` from [`Camera::project`]). Use it to cull distant
    /// peers and, if desired, scale label size with distance.
    pub depth: f32,
}

impl Projected {
    /// Whether the point is within the viewport, allowing a fractional `margin` of NDC slop on every
    /// edge (`0.0` = exactly the frame; `0.1` = keep labels that hang slightly off-edge). A label
    /// that's behind the camera never reaches here — it was culled to `None` already.
    pub fn on_screen(&self, margin: f32) -> bool {
        let limit = 1.0 + margin;
        self.ndc[0].abs() <= limit && self.ndc[1].abs() <= limit
    }
}

/// Smallest view depth (meters) we still project. A point closer than this is treated as "at/behind
/// the camera" and culled — guards the perspective divide against a near-zero / negative `vz`.
const MIN_DEPTH: f32 = 1.0e-3;

impl Camera {
    /// Project a world point to NDC, or `None` if it's at/behind the camera (nothing to draw). The
    /// caller still decides off-screen ([`Projected::on_screen`]) and too-far ([`Projected::depth`])
    /// culling — both need a threshold this pure function shouldn't bake in.
    pub fn project(&self, world: Vec3) -> Option<Projected> {
        // Transform into view space by projecting the camera→point vector onto the camera basis.
        // For an orthonormal frame this is exactly the world→view rotation (no matrix inverse needed).
        let rel = sub(world, self.position);
        let depth = dot(rel, self.forward);
        if depth <= MIN_DEPTH {
            return None; // at or behind the camera plane
        }
        // Guard the degenerate camera params that would divide by zero or blow up the focal length;
        // a live camera never hits these, but a half-initialized one (frame 0) might. A NaN that slips
        // past these (`NaN <= 0.0` is false) yields NaN NDC, which `on_screen` then culls.
        let half_fov = self.fov_y * 0.5;
        let tan_half = half_fov.tan();
        if tan_half <= 0.0 || self.aspect_ratio <= 0.0 {
            return None;
        }
        let focal = 1.0 / tan_half;
        let x = dot(rel, self.right);
        let y = dot(rel, self.up);
        let ndc_x = (x / depth) * focal / self.aspect_ratio;
        let ndc_y = (y / depth) * focal;
        Some(Projected { ndc: [ndc_x, ndc_y], depth })
    }
}

/// Map NDC (`+y` up) to framebuffer pixels (`+y` **down**, origin top-left) for a `[width, height]`
/// viewport. The y-flip is the NDC→screen convention change; the cdylib calls this on the Present
/// thread once it knows the real `display_size`.
pub fn ndc_to_screen(ndc: [f32; 2], viewport: [f32; 2]) -> [f32; 2] {
    [(ndc[0] * 0.5 + 0.5) * viewport[0], (1.0 - (ndc[1] * 0.5 + 0.5)) * viewport[1]]
}

/// Clamp an NDC point to the screen border in the direction of the point from screen-center, for an
/// **off-screen indicator** — a dot pinned to the edge pointing at an off-screen teammate (the design
/// in `docs/NAMEPLATES.md`). A point already within `limit` on both axes is on-screen and returned
/// unchanged; otherwise it's scaled so its larger-magnitude axis lands on `±limit`, putting it on the
/// border along the same bearing from center. `limit` is the NDC half-extent (`1.0` = the exact frame
/// edge; pass slightly under, e.g. `0.95`, to inset the dot so it isn't half off-screen).
///
/// This only handles points that project *in front* of the camera but off to the side. A point
/// **behind** the camera has no valid NDC ([`Camera::project`] returns `None`), so the edge-indicator
/// wiring must derive its bearing separately (e.g. from the peer's view-space direction) before
/// calling this — that part is the 2-player-gated rendering step, not this pure clamp.
pub fn clamp_ndc_to_edge(ndc: [f32; 2], limit: f32) -> [f32; 2] {
    let m = ndc[0].abs().max(ndc[1].abs());
    if m <= limit || m == 0.0 {
        return ndc; // already on-screen (or degenerate dead-center)
    }
    let t = limit / m;
    [ndc[0] * t, ndc[1] * t]
}

/// Default view-depth (meters) at/past which an on-screen nameplate degrades from full text to a small
/// dot — the **distance LOD** in `docs/NAMEPLATES.md`. It's a *second* distance meant to sit inside the
/// `nameplates.max_distance_m` hard cull (a peer past `max_distance_m` draws nothing at all); between
/// this and that cull, the peer reads as a colored dot rather than mushy shrunk text. If the cull is set
/// tighter than this, the dot stage is simply empty and every visible peer stays text. A bare constant
/// for now (not a config knob — the rendering lane doesn't own the config surface); the 2-player rig is
/// where the exact value gets its final tune.
pub const DEFAULT_DOT_DISTANCE_M: f32 = 25.0;

/// Whether a nameplate at `depth` meters (view-space forward distance — [`Projected::depth`]) should
/// render as a **dot** instead of full text: true once the peer is at or beyond `dot_distance_m`. The
/// switch is by depth, not font scaling, because shrinking a bitmap font turns it mushy (see the design
/// doc). A non-finite `depth` (e.g. a torn camera read that produced NaN) returns `false` — degrade to
/// drawing text rather than silently swallowing the label — since `NaN >= x` is false.
pub fn is_dot_lod(depth: f32, dot_distance_m: f32) -> bool {
    depth >= dot_distance_m
}

impl Camera {
    /// Straight-line (radial) distance, in meters, from the camera to `world`. Unlike
    /// [`Projected::depth`] (forward view-Z, undefined for a point behind the camera), this is defined
    /// everywhere, so it's the right metric for the nameplate range cull — one threshold governs both an
    /// on-screen plate and a behind-camera edge indicator. Non-finite if `world` is (a torn position
    /// read), which the caller culls.
    pub fn distance_to(&self, world: Vec3) -> f32 {
        let rel = sub(world, self.position);
        dot(rel, rel).sqrt()
    }

    /// Where on the screen **border** to pin an off-screen indicator dot pointing toward `world` — the
    /// "teammate is over here" co-op compass in `docs/NAMEPLATES.md`. Returns an NDC point on the
    /// `±limit` border along the bearing to the peer (`limit` is the NDC half-extent; pass slightly
    /// under `1.0`, e.g. `0.95`, to inset the dot so it isn't half off-screen).
    ///
    /// Handles the two off-screen cases the bare [`clamp_ndc_to_edge`] can't on its own:
    ///  - **in front but off to the side** (`depth > 0`): project normally, then clamp to the border;
    ///  - **at/behind the camera plane** (`depth <= 0`, where [`project`](Camera::project) returns
    ///    `None`): there's no perspective image, so derive the bearing from the view-space *lateral*
    ///    offset (`right`/`up` components) and pin that to the border. This is continuous with the
    ///    in-front case — as a peer crosses from just-in-front to just-behind on a given side, the dot
    ///    stays on that same edge rather than jumping. A peer dead behind has no lateral bearing, so it
    ///    defaults to the bottom edge ("turn around"). The exact behind-camera feel is 2-player-tuned.
    pub fn edge_indicator_ndc(&self, world: Vec3, limit: f32) -> [f32; 2] {
        let rel = sub(world, self.position);
        let depth = dot(rel, self.forward);
        let tan_half = (self.fov_y * 0.5).tan();
        // Degenerate camera params (the `is_finite` checks also catch a NaN fov, which the `<= 0.0`
        // guards alone would let slip through): no usable bearing, so fall back to the bottom edge
        // rather than dividing by zero / propagating NaN.
        if !tan_half.is_finite() || tan_half <= 0.0 || !self.aspect_ratio.is_finite() || self.aspect_ratio <= 0.0 {
            return [0.0, -limit];
        }
        let focal = 1.0 / tan_half;
        // View-space lateral offset, pre-divide, with the same focal/aspect weighting `project` uses —
        // so for an in-front point `[ax/depth, ay/depth]` is exactly its NDC.
        let ax = dot(rel, self.right) * focal / self.aspect_ratio;
        let ay = dot(rel, self.up) * focal;
        if depth > MIN_DEPTH {
            clamp_ndc_to_edge([ax / depth, ay / depth], limit)
        } else {
            // Behind/beside the camera: no divide. Point at the lateral bearing, pinned to the border;
            // a dead-behind peer (no lateral offset) — or a non-finite world point (a torn position read
            // gives NaN here) — has no usable bearing, so default to the bottom edge rather than emit a
            // NaN NDC that would reach the draw list.
            let m = ax.abs().max(ay.abs());
            if !m.is_finite() || m <= 0.0 { [0.0, -limit] } else { [ax / m * limit, ay / m * limit] }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned camera at the origin looking down `+z`, 90° vertical fov (so `tan(45°) = 1`
    /// and the focal length is 1 — the cleanest numbers to reason about), square viewport.
    fn cam_origin(aspect: f32) -> Camera {
        Camera {
            position: [0.0, 0.0, 0.0],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            forward: [0.0, 0.0, 1.0],
            fov_y: std::f32::consts::FRAC_PI_2, // 90°
            aspect_ratio: aspect,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1.0e-5
    }

    #[test]
    fn point_dead_ahead_maps_to_center() {
        let p = cam_origin(1.0).project([0.0, 0.0, 10.0]).expect("in front");
        assert!(close(p.ndc[0], 0.0) && close(p.ndc[1], 0.0), "{:?}", p.ndc);
        assert!(close(p.depth, 10.0), "depth {}", p.depth);
    }

    #[test]
    fn frustum_edges_map_to_ndc_units() {
        let cam = cam_origin(1.0);
        // At 90° fov and depth d, the top/right frustum edge is at offset d.
        let right = cam.project([10.0, 0.0, 10.0]).unwrap();
        assert!(close(right.ndc[0], 1.0), "right edge {:?}", right.ndc);
        let top = cam.project([0.0, 10.0, 10.0]).unwrap();
        assert!(close(top.ndc[1], 1.0), "top edge {:?}", top.ndc);
    }

    #[test]
    fn aspect_ratio_compresses_horizontal_ndc() {
        // A wider viewport (aspect 2) puts the same lateral world offset at half the NDC x.
        let p = cam_origin(2.0).project([10.0, 0.0, 10.0]).unwrap();
        assert!(close(p.ndc[0], 0.5), "{:?}", p.ndc);
    }

    #[test]
    fn behind_camera_is_culled() {
        let cam = cam_origin(1.0);
        assert!(cam.project([0.0, 0.0, -10.0]).is_none(), "directly behind");
        assert!(cam.project([0.0, 0.0, 0.0]).is_none(), "at the camera plane");
        assert!(cam.project([5.0, 5.0, -0.001]).is_none(), "just behind");
    }

    #[test]
    fn degenerate_camera_params_are_culled_not_panicked() {
        let mut cam = cam_origin(1.0);
        cam.aspect_ratio = 0.0;
        assert!(cam.project([0.0, 0.0, 10.0]).is_none(), "zero aspect");
        let mut cam = cam_origin(1.0);
        cam.fov_y = 0.0;
        assert!(cam.project([0.0, 0.0, 10.0]).is_none(), "zero fov");
    }

    #[test]
    fn rotated_camera_projects_along_its_own_basis() {
        // Camera looking down +x, with a non-trivial right/up frame (right = world -z, up = world +y).
        let cam = Camera {
            position: [5.0, 1.0, 0.0],
            right: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            forward: [1.0, 0.0, 0.0],
            fov_y: std::f32::consts::FRAC_PI_2, // focal = 1
            aspect_ratio: 1.0,
        };
        // A point straight ahead along +forward lands dead center (the easy half).
        let ahead = cam.project([15.0, 1.0, 0.0]).expect("in front");
        assert!(close(ahead.ndc[0], 0.0) && close(ahead.ndc[1], 0.0), "{:?}", ahead.ndc);
        assert!(close(ahead.depth, 10.0), "depth {}", ahead.depth);

        // The discriminating half: offset the point along the camera's *rotated* right and up by
        // ASYMMETRIC amounts (right edge, half-up) at depth 10. rel = [10, 5, -10] → depth = rel·forward
        // = 10, rel·right = -rel.z = 10 → ndc_x = +1, rel·up = rel.y = 5 → ndc_y = +0.5. Asymmetric so a
        // swapped/negated/transposed right⇄up basis would move the asserted NDC — unlike a dead-ahead
        // point, whose zero offsets vanish regardless of what right/up are.
        let off = cam.project([15.0, 6.0, -10.0]).expect("in front");
        assert!(close(off.ndc[0], 1.0), "rotated right edge: {:?}", off.ndc);
        assert!(close(off.ndc[1], 0.5), "rotated half-up: {:?}", off.ndc);
        assert!(close(off.depth, 10.0), "depth {}", off.depth);
    }

    #[test]
    fn near_plane_positive_depth_is_culled() {
        // A point in front but nearer than MIN_DEPTH (the w≈0 degenerate the constant guards) culls,
        // rather than dividing by a near-zero depth and flinging the label to infinity.
        let cam = cam_origin(1.0);
        assert!(cam.project([0.0, 0.0, MIN_DEPTH * 0.5]).is_none(), "inside the near plane");
        assert!(cam.project([0.0, 0.0, MIN_DEPTH * 2.0]).is_some(), "just outside it projects");
    }

    #[test]
    fn nan_fov_culls_via_on_screen_not_project() {
        // A NaN fov (e.g. a torn matrix read) slips past project()'s `<= 0` guards — `NaN <= 0.0` is
        // false — so project yields NaN NDC rather than `None` (its documented contract). The cull then
        // happens in `on_screen` (`abs(NaN) <= limit` is false), so a NaN never silently draws a label
        // at a bogus pixel. This pins both halves of that contract.
        let mut cam = cam_origin(1.0);
        cam.fov_y = f32::NAN;
        let p = cam.project([0.0, 0.0, 10.0]).expect("NaN fov still returns Some, culled downstream");
        assert!(p.ndc[0].is_nan() && p.ndc[1].is_nan(), "expected NaN ndc, got {:?}", p.ndc);
        assert!(!p.on_screen(0.1), "NaN ndc must not be on-screen");
    }

    #[test]
    fn on_screen_respects_margin() {
        let just_off = Projected { ndc: [1.05, 0.0], depth: 5.0 };
        assert!(!just_off.on_screen(0.0), "outside the exact frame");
        assert!(just_off.on_screen(0.1), "inside a 0.1 margin");
        let centered = Projected { ndc: [0.0, 0.0], depth: 5.0 };
        assert!(centered.on_screen(0.0));
    }

    #[test]
    fn clamp_to_edge_pins_offscreen_points_to_the_border() {
        // On-screen points pass through untouched.
        assert_eq!(clamp_ndc_to_edge([0.5, -0.5], 1.0), [0.5, -0.5]);
        assert_eq!(clamp_ndc_to_edge([0.0, 0.0], 1.0), [0.0, 0.0]); // dead center, no divide-by-zero
        // Off one axis → that axis hits ±limit, the other stays proportional (same bearing from center).
        assert_eq!(clamp_ndc_to_edge([2.0, 0.0], 1.0), [1.0, 0.0]);
        assert_eq!(clamp_ndc_to_edge([0.0, -3.0], 1.0), [0.0, -1.0]);
        assert_eq!(clamp_ndc_to_edge([3.0, 1.5], 1.0), [1.0, 0.5]); // x dominates → x=1, y scaled
        assert_eq!(clamp_ndc_to_edge([2.0, 2.0], 1.0), [1.0, 1.0]); // corner stays on the diagonal
        // A sub-1.0 limit insets the dot from the exact edge.
        assert_eq!(clamp_ndc_to_edge([2.0, 0.0], 0.9), [0.9, 0.0]);
    }

    #[test]
    fn is_dot_lod_switches_at_the_threshold() {
        // Text up close, dot at/past the threshold (inclusive boundary).
        assert!(!is_dot_lod(10.0, 25.0), "near peer stays text");
        assert!(!is_dot_lod(24.999, 25.0), "just inside the threshold is still text");
        assert!(is_dot_lod(25.0, 25.0), "at the threshold is a dot");
        assert!(is_dot_lod(60.0, 25.0), "far peer is a dot");
        // A non-finite depth degrades to text (draw something) rather than a dot — `NaN >= x` is false.
        assert!(!is_dot_lod(f32::NAN, 25.0), "NaN depth must not read as a dot");
    }

    #[test]
    fn edge_indicator_pins_offscreen_front_points_to_the_border() {
        let cam = cam_origin(1.0);
        // In front but far off to the right (vx ≫ depth): right edge, vertically centered.
        let r = cam.edge_indicator_ndc([100.0, 0.0, 1.0], 0.95);
        assert!(close(r[0], 0.95) && close(r[1], 0.0), "right edge: {r:?}");
        // In front and up-left: clamps along the diagonal, larger axis at the limit.
        let ul = cam.edge_indicator_ndc([-50.0, 100.0, 1.0], 0.95);
        assert!(close(ul[1], 0.95), "top dominates: {ul:?}");
        assert!(ul[0] < 0.0 && ul[0].abs() <= 0.95, "left-of-center, within border: {ul:?}");
    }

    #[test]
    fn edge_indicator_derives_a_bearing_for_behind_camera_points() {
        let cam = cam_origin(1.0); // looks down +z
        // Dead behind, no lateral offset → defaults to the bottom edge ("turn around").
        assert_eq!(cam.edge_indicator_ndc([0.0, 0.0, -10.0], 0.95), [0.0, -0.95]);
        // Behind and to the right → right edge (project() would have returned None here).
        assert!(cam.project([5.0, 0.0, -10.0]).is_none(), "precondition: behind the camera");
        let br = cam.edge_indicator_ndc([5.0, 0.0, -10.0], 0.95);
        assert!(close(br[0], 0.95) && close(br[1], 0.0), "behind-right → right edge: {br:?}");
        // Behind and above → top edge.
        let ba = cam.edge_indicator_ndc([0.0, 5.0, -10.0], 0.95);
        assert!(close(ba[0], 0.0) && close(ba[1], 0.95), "behind-above → top edge: {ba:?}");
    }

    #[test]
    fn edge_indicator_is_continuous_across_the_camera_plane() {
        // A peer on the right side stays on the right edge whether just in front of or just behind the
        // camera plane — the dot doesn't flip sides as they cross it (the behind-camera continuity the
        // method's docs promise).
        let cam = cam_origin(1.0);
        let front = cam.edge_indicator_ndc([5.0, 0.0, MIN_DEPTH * 2.0], 0.95);
        let behind = cam.edge_indicator_ndc([5.0, 0.0, -MIN_DEPTH * 2.0], 0.95);
        assert!(front[0] > 0.0 && behind[0] > 0.0, "both on the right edge: {front:?} {behind:?}");
    }

    #[test]
    fn edge_indicator_falls_back_on_degenerate_camera() {
        let mut cam = cam_origin(1.0);
        cam.fov_y = f32::NAN; // a torn matrix read
        let p = cam.edge_indicator_ndc([10.0, 0.0, 10.0], 0.95);
        assert_eq!(p, [0.0, -0.95], "degenerate camera → bottom-edge fallback, no NaN");
    }

    #[test]
    fn edge_indicator_falls_back_on_non_finite_world() {
        // A torn position read (NaN world coord) must not emit a NaN NDC that reaches the draw list —
        // it degrades to the bottom-edge fallback, the same as a degenerate camera. (The feature also
        // culls a non-finite distance upstream; this is the in-math backstop.)
        let cam = cam_origin(1.0);
        let p = cam.edge_indicator_ndc([f32::NAN, 0.0, -10.0], 0.95);
        assert!(p[0].is_finite() && p[1].is_finite(), "must not emit NaN NDC: {p:?}");
        assert_eq!(p, [0.0, -0.95]);
    }

    #[test]
    fn distance_to_is_radial_and_non_finite_passes_through() {
        let cam = cam_origin(1.0); // at the origin
        assert!(close(cam.distance_to([3.0, 4.0, 0.0]), 5.0), "3-4-5 radial distance");
        assert!(close(cam.distance_to([0.0, 0.0, -10.0]), 10.0), "behind the camera still has a distance");
        assert!(!cam.distance_to([f32::NAN, 0.0, 0.0]).is_finite(), "NaN world → non-finite distance to cull");
    }

    #[test]
    fn ndc_to_screen_maps_corners() {
        let vp = [1920.0, 1080.0];
        assert_eq!(ndc_to_screen([0.0, 0.0], vp), [960.0, 540.0]); // center
        assert_eq!(ndc_to_screen([-1.0, 1.0], vp), [0.0, 0.0]); // NDC top-left → pixel origin
        assert_eq!(ndc_to_screen([1.0, -1.0], vp), [1920.0, 1080.0]); // NDC bottom-right → far corner
    }
}
