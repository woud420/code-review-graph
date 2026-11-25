use std::collections::BTreeSet;

// NumPy's legacy RandomState(seed).rand(): MT19937, with two draws per f64.
struct RandomState {
    words: [u32; 624],
    index: usize,
}
impl RandomState {
    fn new(seed: u32) -> Self {
        let mut words = [0u32; 624];
        words[0] = seed;
        for i in 1..624 {
            words[i] = 1812433253u32
                .wrapping_mul(words[i - 1] ^ (words[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Self { words, index: 624 }
    }
    fn draw(&mut self) -> u32 {
        if self.index == 624 {
            for i in 0..624 {
                let y = (self.words[i] & 0x8000_0000) | (self.words[(i + 1) % 624] & 0x7fff_ffff);
                self.words[i] = self.words[(i + 397) % 624]
                    ^ (y >> 1)
                    ^ if y & 1 == 1 { 0x9908_b0df } else { 0 };
            }
            self.index = 0;
        }
        let mut y = self.words[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }
    fn unit(&mut self) -> f64 {
        (((self.draw() >> 5) as u64 * 67_108_864 + (self.draw() >> 6) as u64) as f64)
            / 9_007_199_254_740_992.0
    }
}

type Point = [f64; 2];

fn bounds(points: &[Point]) -> (Point, Point) {
    points.iter().fold(
        ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]),
        |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1])],
                [hi[0].max(p[0]), hi[1].max(p[1])],
            )
        },
    )
}

pub(super) fn positions(edges: &[BTreeSet<usize>]) -> Vec<Point> {
    let n = edges.len();
    if n < 2 {
        return vec![[450., 345.]; n];
    }
    let mut rng = RandomState::new(42);
    let mut positions = (0..n).map(|_| [rng.unit(), rng.unit()]).collect::<Vec<_>>();
    let (lo, hi) = bounds(&positions);
    let mut temperature = (hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.1;
    let cooling = temperature / 51.;
    let k = 2. / (n as f64).sqrt();
    // For large graphs a deterministic rotating sample bounds repulsion work and
    // all directed links still contribute their individual attractive forces.
    let sample = if n < 500 { n } else { 128 };
    for iteration in 0..50 {
        let mut next = positions.clone();
        let mut norm = 0.;
        for (i, &[x, y]) in positions.iter().enumerate() {
            let mut force = [0., 0.];
            let offset = (i + iteration * 41) % n;
            for j in 0..sample {
                let other = if n == sample {
                    j
                } else {
                    (j * n / sample + offset) % n
                };
                let [ox, oy] = positions[other];
                let (dx, dy) = (x - ox, y - oy);
                let distance = dx.hypot(dy).max(0.01);
                let strength = k * k / distance.powi(2) * (n as f64 / sample as f64);
                force[0] += dx * strength;
                force[1] += dy * strength;
            }
            for &other in &edges[i] {
                let [ox, oy] = positions[other];
                let (dx, dy) = (x - ox, y - oy);
                let strength = dx.hypot(dy).max(0.01) / k;
                force[0] -= dx * strength;
                force[1] -= dy * strength;
            }
            let step = temperature / force[0].hypot(force[1]).max(0.01);
            let (dx, dy) = (force[0] * step, force[1] * step);
            next[i] = [x + dx, y + dy];
            norm += dx * dx + dy * dy;
        }
        positions = next;
        temperature -= cooling;
        if norm.sqrt() / (n as f64) < 1e-4 {
            break;
        }
    }
    let (lo, hi) = bounds(&positions);
    let proportion = |v: f64, a: f64, b: f64| if a == b { 0.5 } else { (v - a) / (b - a) };
    for p in &mut positions {
        *p = [
            40. + 820. * proportion(p[0], lo[0], hi[0]),
            640. - 590. * proportion(p[1], lo[1], hi[1]),
        ];
    }
    positions
}
