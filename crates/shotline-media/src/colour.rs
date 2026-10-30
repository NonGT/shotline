//! Colour matching between clips: one fixed Monge-Kantorovich linear transform (Pitie 2007) per
//! clip, from the stats of its first frames to those of the previous clip's last frames. Being
//! fixed for the whole clip, it doesn't flicker.

type M3 = [[f64; 3]; 3];
type V3 = [f64; 3];

/// Mean and covariance of RGB in 0..1, every 4th pixel of each frame.
pub fn stats<'a>(frames: impl IntoIterator<Item = &'a [u8]>) -> (V3, M3) {
    let (mut n, mut sum, mut sq) = (0.0, [0.0; 3], [[0.0; 3]; 3]);
    for f in frames {
        for px in f.as_chunks::<3>().0.iter().step_by(4) {
            let x = [
                px[0] as f64 / 255.0,
                px[1] as f64 / 255.0,
                px[2] as f64 / 255.0,
            ];
            n += 1.0;
            for i in 0..3 {
                sum[i] += x[i];
                for j in 0..3 {
                    sq[i][j] += x[i] * x[j];
                }
            }
        }
    }
    let n = f64::max(n, 1.0);
    let mean = sum.map(|s| s / n);
    let mut cov = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            cov[i][j] = sq[i][j] / n - mean[i] * mean[j];
        }
        cov[i][i] += 1e-6;
    }
    (mean, cov)
}

fn mul(a: &M3, b: &M3) -> M3 {
    let mut c = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            c[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    c
}

fn transpose(a: &M3) -> M3 {
    let mut t = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            t[i][j] = a[j][i];
        }
    }
    t
}

/// Eigenvalues and eigenvectors (as columns) of a symmetric matrix, by Jacobi rotations.
fn eigh(m: &M3) -> (V3, M3) {
    let mut a = *m;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off < 1e-15 {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-18 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            let mut r = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
            r[p][p] = c;
            r[q][q] = c;
            r[p][q] = s;
            r[q][p] = -s;
            a = mul(&transpose(&r), &mul(&a, &r));
            v = mul(&v, &r);
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}

fn sqrtm_psd(m: &M3) -> M3 {
    let (vals, vecs) = eigh(m);
    let mut d = [[0.0; 3]; 3];
    for i in 0..3 {
        d[i][i] = vals[i].max(1e-12).sqrt();
    }
    mul(&vecs, &mul(&d, &transpose(&vecs)))
}

fn inverse(m: &M3) -> M3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let k = 1.0 / det;
    [
        [
            (e * i - f * h) * k,
            (c * h - b * i) * k,
            (b * f - c * e) * k,
        ],
        [
            (f * g - d * i) * k,
            (a * i - c * g) * k,
            (c * d - a * f) * k,
        ],
        [
            (d * h - e * g) * k,
            (b * g - a * h) * k,
            (a * e - b * d) * k,
        ],
    ]
}

/// T with T · src · Tᵀ = tgt.
fn mkl(src: &M3, tgt: &M3) -> M3 {
    let a = sqrtm_psd(src);
    let a_inv = inverse(&a);
    mul(&a_inv, &mul(&sqrtm_psd(&mul(&a, &mul(tgt, &a))), &a_inv))
}

/// `y = T (x - mean_src) + mean_tgt`, blended with `x` by `strength`.
#[derive(Debug, Clone)]
pub struct Transform {
    m: [[f32; 3]; 3],
    offset: [f32; 3],
}

impl Transform {
    /// Maps frames that look like `src` onto `tgt`.
    pub fn between(src: (V3, M3), tgt: (V3, M3), strength: f64) -> Transform {
        let t = mkl(&src.1, &tgt.1);
        let s = strength.clamp(0.0, 1.0);
        // Blending y with x is the same as blending the matrix with I and the offset with 0.
        let mut m = [[0.0f32; 3]; 3];
        let mut offset = [0.0f32; 3];
        for i in 0..3 {
            let mut o = tgt.0[i];
            for j in 0..3 {
                let id = if i == j { 1.0 } else { 0.0 };
                m[i][j] = (id + (t[i][j] - id) * s) as f32;
                o -= t[i][j] * src.0[j];
            }
            offset[i] = (o * s) as f32;
        }
        Transform { m, offset }
    }

    /// Changes an RGB24 frame in place.
    pub fn apply(&self, frame: &mut [u8]) {
        let (m, o) = (&self.m, &self.offset);
        for px in frame.as_chunks_mut::<3>().0 {
            let x = [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ];
            for i in 0..3 {
                let y = m[i][0] * x[0] + m[i][1] * x[1] + m[i][2] * x[2] + o[i];
                px[i] = (y.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &M3, b: &M3) -> bool {
        (0..3).all(|i| (0..3).all(|j| (a[i][j] - b[i][j]).abs() < 1e-9))
    }

    const COV: M3 = [
        [0.04, 0.01, 0.002],
        [0.01, 0.03, 0.005],
        [0.002, 0.005, 0.02],
    ];

    #[test]
    fn square_root() {
        let r = sqrtm_psd(&COV);
        assert!(close(&mul(&r, &r), &COV));
    }

    #[test]
    fn mkl_maps_one_covariance_onto_the_other() {
        let tgt = [[0.02, -0.004, 0.0], [-0.004, 0.05, 0.01], [0.0, 0.01, 0.03]];
        let t = mkl(&COV, &tgt);
        assert!(close(&mul(&t, &mul(&COV, &transpose(&t))), &tgt));
    }

    #[test]
    fn moves_the_mean() {
        // A reddish frame matched to a bluish one comes out bluish.
        let red: Vec<u8> = (0..300)
            .flat_map(|i| [200, 80 + (i % 7) as u8, 60])
            .collect();
        let blue: Vec<u8> = (0..300)
            .flat_map(|i| [60, 90 + (i % 5) as u8, 190])
            .collect();
        let t = Transform::between(stats([&red[..]]), stats([&blue[..]]), 1.0);
        let mut f = red.clone();
        t.apply(&mut f);
        let (m, _) = stats([&f[..]]);
        let (want, _) = stats([&blue[..]]);
        for i in 0..3 {
            assert!((m[i] - want[i]).abs() < 0.02, "{m:?} vs {want:?}");
        }
        let none = Transform::between(stats([&red[..]]), stats([&blue[..]]), 0.0);
        let mut g = red.clone();
        none.apply(&mut g);
        assert_eq!(g, red);
    }
}
