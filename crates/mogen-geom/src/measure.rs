use glam::Vec3;

/// Closest points on two finite segments, including point segments.
pub fn closest_segment_points(p: Vec3, q: Vec3, r: Vec3, s: Vec3) -> (Vec3, Vec3) {
    let p = p.as_dvec3();
    let q = q.as_dvec3();
    let r = r.as_dvec3();
    let s = s.as_dvec3();
    let u = q - p;
    let v = s - r;
    let w = p - r;
    let a = u.dot(u);
    let b = u.dot(v);
    let c = v.dot(v);
    let d = u.dot(w);
    let e = v.dot(w);
    let (mut x, y);
    if a == 0.0 && c == 0.0 {
        return (p.as_vec3(), r.as_vec3());
    }
    if a == 0.0 {
        x = 0.0;
        y = (e / c).clamp(0.0, 1.0);
    } else if c == 0.0 {
        x = (-d / a).clamp(0.0, 1.0);
        y = 0.0;
    } else {
        let denom = a * c - b * b;
        x = if denom > f64::EPSILON * a * c {
            (b * e - c * d) / denom
        } else {
            0.0
        };
        x = x.clamp(0.0, 1.0);
        let t = (b * x + e) / c;
        if t < 0.0 {
            y = 0.0;
            x = (-d / a).clamp(0.0, 1.0);
        } else if t > 1.0 {
            y = 1.0;
            x = ((b - d) / a).clamp(0.0, 1.0);
        } else {
            y = t;
        }
    }
    ((p + u * x).as_vec3(), (r + v * y).as_vec3())
}
