//! World→screen projection math — pure, **host-tested**, no game/SDK/OS deps.
//!
//! This is the math behind the overhead peer nameplates (see `docs/OVERLAY-RENDERING.md` >
//! "(Later) Overhead nameplates"): given the camera and a peer's world position, where on screen
//! does a label go, and should it be drawn at all? Keeping it here means the tricky part — the
//! perspective transform and the culling — is verified on the Mac (`scripts/test-core.sh`); the
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
    fn ndc_to_screen_maps_corners() {
        let vp = [1920.0, 1080.0];
        assert_eq!(ndc_to_screen([0.0, 0.0], vp), [960.0, 540.0]); // center
        assert_eq!(ndc_to_screen([-1.0, 1.0], vp), [0.0, 0.0]); // NDC top-left → pixel origin
        assert_eq!(ndc_to_screen([1.0, -1.0], vp), [1920.0, 1080.0]); // NDC bottom-right → far corner
    }
}
