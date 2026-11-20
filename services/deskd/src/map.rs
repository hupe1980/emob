//! Maps drawn on the server (spec 022): the fleet on the Stations page, one
//! station on its asset card.
//!
//! The outline is the bundled Natural Earth one ([`crate::europe`]), so a map
//! costs no tile server, no script and no third party (SR-153). It is
//! projected equirectangularly with the longitude scaled by the cosine of the
//! view's middle latitude, which is true enough at the scale of a country and
//! needs no float: every coordinate is a whole hundredth of a degree, every
//! cosine a whole thousandth.
//!
//! The map is the picture; the table beside it is the list. Its dots and cells
//! are links for the mouse and kept out of the tab order, because the table
//! already carries every link once for the keyboard.

use core::fmt::Write as _;

use emob_service::ui::{Lang, escape};
use rust_decimal::Decimal;

use crate::europe::RINGS;

/// The view's width in SVG units; its height follows the projection.
const WIDTH: i64 = 1000;

/// `cos(φ)` in thousandths for whole degrees 0–90.
const COS: [i64; 91] = [
    1000, 1000, 999, 999, 998, 996, 995, 993, 990, 988, 985, 982, 978, 974, 970, 966, 961, 956,
    951, 946, 940, 934, 927, 921, 914, 906, 899, 891, 883, 875, 866, 857, 848, 839, 829, 819, 809,
    799, 788, 777, 766, 755, 743, 731, 719, 707, 695, 682, 669, 656, 643, 629, 616, 602, 588, 574,
    559, 545, 530, 515, 500, 485, 469, 454, 438, 423, 407, 391, 375, 358, 342, 326, 309, 292, 276,
    259, 242, 225, 208, 191, 174, 156, 139, 122, 105, 87, 70, 52, 35, 17, 0,
];

/// Decimal degrees as whole hundredths, rounded down; `None` for text that is
/// not a number.
#[must_use]
pub fn hundredths(text: &str) -> Option<i64> {
    use rust_decimal::prelude::ToPrimitive as _;
    let degrees: Decimal = text.trim().parse().ok()?;
    (degrees * Decimal::ONE_HUNDRED).floor().to_i64()
}

/// A rectangle of the earth in hundredths of a degree, and how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    west: i64,
    south: i64,
    east: i64,
    north: i64,
    height: i64,
}

impl View {
    /// The smallest view holding `points`, widened by a margin and to an
    /// aspect a page can hold, and at least `span` hundredths tall.
    fn fit(points: &[(i64, i64)], span: i64) -> Option<Self> {
        let (mut west, mut east) = extent(points.iter().map(|p| p.0))?;
        let (mut south, mut north) = extent(points.iter().map(|p| p.1))?;
        let grow = |lo: &mut i64, hi: &mut i64, to: i64| {
            let more = (to - (*hi - *lo)).max(0);
            *lo -= more / 2;
            *hi += more - more / 2;
        };
        grow(&mut south, &mut north, span);
        let margin = (north - south) / 8;
        south -= margin;
        north += margin;
        let k = cos(i64::midpoint(south, north));
        let margin = (east - west) / 8;
        west -= margin;
        east += margin;
        // Always 2:1, as wide as the column and half as tall, so the page
        // never letterboxes it: whichever side is short grows.
        let tall = (north - south) * 1000;
        let wide = (east - west) * k;
        if wide < tall * 2 {
            grow(&mut west, &mut east, tall * 2 / k + 1);
        } else {
            grow(&mut south, &mut north, wide / 2000 + 1);
        }
        let k = cos(i64::midpoint(south, north));
        let height = (north - south) * 1000 * WIDTH / ((east - west) * k).max(1);
        Some(Self {
            west,
            south,
            east,
            north,
            height: height.max(1),
        })
    }

    fn x(&self, lon: i64) -> i64 {
        (lon - self.west) * WIDTH / (self.east - self.west).max(1)
    }

    fn y(&self, lat: i64) -> i64 {
        (self.north - lat) * self.height / (self.north - self.south).max(1)
    }

    /// The outline inside this view: rings that miss it are left out, and
    /// every other ring is clipped to it — so the fill is exact at the edges,
    /// nothing is drawn past the frame, and Russia's coast costs nothing on a
    /// map of Bavaria.
    fn land(&self) -> String {
        let mut path = String::new();
        for (_, ring) in RINGS {
            let points: Vec<(i64, i64)> = ring
                .iter()
                .map(|&(lon, lat)| (i64::from(lon), i64::from(lat)))
                .collect();
            let clipped = clip(&points, (self.west, self.south, self.east, self.north));
            let mut last: Option<(i64, i64)> = None;
            let mut drawn = 0;
            for (lon, lat) in clipped {
                let at = (self.x(lon), self.y(lat));
                match last {
                    None => {
                        let _ = write!(path, "M{} {}", at.0, at.1);
                    }
                    Some(prev) if prev == at => continue,
                    Some(prev) => {
                        let _ = write!(path, "l{} {}", at.0 - prev.0, at.1 - prev.1);
                    }
                }
                last = Some(at);
                drawn += 1;
            }
            if drawn > 2 {
                path.push('z');
            }
        }
        path
    }

    fn open(&self, label: &str) -> String {
        format!(
            "<svg viewBox=\"0 0 {WIDTH} {h}\" role=\"group\" aria-label=\"{label}\" \
             xmlns=\"http://www.w3.org/2000/svg\"><path class=\"land\" d=\"{land}\"/>",
            h = self.height,
            label = escape(label),
            land = self.land(),
        )
    }
}

/// A ring clipped to a box `(west, south, east, north)`, one edge of the box
/// at a time (Sutherland–Hodgman), in whole hundredths: a ring outside the box
/// comes back empty, one inside comes back as it was.
fn clip(ring: &[(i64, i64)], (west, south, east, north): (i64, i64, i64, i64)) -> Vec<(i64, i64)> {
    // Each edge: whether a point is inside it, and where a segment crosses it.
    type Inside = fn((i64, i64), i64) -> bool;
    type Cross = fn((i64, i64), (i64, i64), i64) -> (i64, i64);
    let at_x: Cross = |a, b, x| (x, a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0));
    let at_y: Cross = |a, b, y| (a.0 + (b.0 - a.0) * (y - a.1) / (b.1 - a.1), y);
    let edges: [(Inside, Cross, i64); 4] = [
        (|p, x| p.0 >= x, at_x, west),
        (|p, x| p.0 <= x, at_x, east),
        (|p, y| p.1 >= y, at_y, south),
        (|p, y| p.1 <= y, at_y, north),
    ];
    let mut points = ring.to_vec();
    for (inside, cross, line) in edges {
        let Some(&last) = points.last() else {
            break;
        };
        let mut out = Vec::with_capacity(points.len());
        let mut prev = last;
        for &point in &points {
            match (inside(prev, line), inside(point, line)) {
                (true, true) => out.push(point),
                (true, false) => out.push(cross(prev, point, line)),
                (false, true) => {
                    out.push(cross(prev, point, line));
                    out.push(point);
                }
                (false, false) => {}
            }
            prev = point;
        }
        points = out;
    }
    points
}

fn cos(lat: i64) -> i64 {
    let degrees = (lat.abs() / 100).min(90);
    COS[usize::try_from(degrees).unwrap_or(90)]
}

fn extent(values: impl Iterator<Item = i64>) -> Option<(i64, i64)> {
    values.fold(None, |range, v| {
        Some(range.map_or((v, v), |(lo, hi): (i64, i64)| (lo.min(v), hi.max(v))))
    })
}

/// One station on the fleet map.
#[derive(Debug, Clone)]
pub struct Dot {
    /// Its identity.
    pub identity: String,
    /// Longitude, hundredths.
    pub lon: i64,
    /// Latitude, hundredths.
    pub lat: i64,
    /// `online`, `offline`, `unknown`.
    pub presence: String,
}

/// One cell of the fleet grid.
#[derive(Debug, Clone)]
pub struct Cell {
    /// `west,south,east,north`, as the search takes it.
    pub area: String,
    /// Its bounds, hundredths.
    pub bounds: (i64, i64, i64, i64),
    /// Stations inside.
    pub stations: u64,
    /// Of them, silent.
    pub silent: u64,
}

/// The fleet map: a dot per station, or a grid whose cells link to the search
/// narrowed to them. `query` is the search it draws, carried into each cell's
/// link. Empty when there is nothing to draw.
#[must_use]
pub fn fleet(dots: &[Dot], cells: &[Cell], query: &str, lang: Lang) -> String {
    let mut corners: Vec<(i64, i64)> = dots.iter().map(|d| (d.lon, d.lat)).collect();
    for cell in cells {
        corners.push((cell.bounds.0, cell.bounds.1));
        corners.push((cell.bounds.2, cell.bounds.3));
    }
    let Some(view) = View::fit(&corners, 500) else {
        return String::new();
    };
    let (online, silent) = dots
        .iter()
        .fold((0, 0), |(on, off), dot| match dot.presence.as_str() {
            "online" => (on + 1, off),
            "offline" => (on, off + 1),
            _ => (on, off),
        });
    let label = if cells.is_empty() {
        match lang {
            Lang::De => format!(
                "Karte: {} Stationen, {online} online, {silent} still",
                dots.len()
            ),
            Lang::En => format!(
                "Map: {} stations, {online} online, {silent} silent",
                dots.len()
            ),
        }
    } else {
        lang.pick(
            "Karte: Stationen je Feld — ein Feld öffnet seine Stationen",
            "Map: stations per cell — a cell opens its stations",
        )
        .to_owned()
    };
    let mut svg = view.open(&label);
    // Silent last, so a silent dot is never hidden under a talking one.
    let mut ordered: Vec<&Dot> = dots.iter().collect();
    ordered.sort_by_key(|dot| dot.presence == "offline");
    for dot in ordered {
        let _ = write!(
            svg,
            "<a href=\"{href}\" tabindex=\"-1\" aria-label=\"{id}\"><circle class=\"dot {presence}\" cx=\"{x}\" cy=\"{y}\" r=\"9\"><title>{id}</title></circle></a>",
            href = escape(&crate::station::href(&dot.identity)),
            id = escape(&dot.identity),
            presence = match dot.presence.as_str() {
                "online" => "online",
                "offline" => "offline",
                _ => "unknown",
            },
            x = view.x(dot.lon),
            y = view.y(dot.lat),
        );
    }
    let most = cells.iter().map(|c| c.stations).max().unwrap_or(1).max(1);
    for cell in cells {
        let (w, s, e, n) = cell.bounds;
        let (cx, cy) = (view.x(i64::midpoint(w, e)), view.y(i64::midpoint(s, n)));
        // Area, not radius, follows the count.
        let radius = |count: u64| 6 + (count * 400 / most).isqrt();
        let title = match lang {
            Lang::De => format!("{} Stationen, davon {} still", cell.stations, cell.silent),
            Lang::En => format!("{} stations, {} silent", cell.stations, cell.silent),
        };
        let _ = write!(
            svg,
            "<a href=\"/stations?area={area}&amp;q={q}\" tabindex=\"-1\" aria-label=\"{title}\">\
             <circle class=\"cell\" cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\"><title>{title}</title></circle>",
            area = escape(&cell.area),
            q = escape(&query.replace(' ', "+")),
            r = radius(cell.stations),
        );
        if cell.silent > 0 {
            let _ = write!(
                svg,
                "<circle class=\"dot offline\" cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\"/>",
                r = (radius(cell.silent) - 2).max(4),
            );
        }
        svg.push_str("</a>");
    }
    svg.push_str("</svg>");
    let legend = if cells.is_empty() {
        format!(
            "<span class=\"legend online\">● {on}</span> <span class=\"legend offline\">◌ {off}</span> <span class=\"legend unknown\">○ {never}</span>",
            on = lang.pick("online", "online"),
            off = lang.pick("still", "silent"),
            never = lang.pick("nie gehört", "never heard"),
        )
    } else {
        lang.pick(
            "Kreisfläche nach Anzahl, innen der stille Teil. Ein Kreis öffnet sein Feld.",
            "Circle area by count, the silent share inside. A circle opens its cell.",
        )
        .to_owned()
    };
    format!(
        "<figure class=\"map\">{svg}<figcaption>{legend} <span class=\"credit\">{credit}</span></figcaption></figure>",
        credit = lang.pick("Umrisse: Natural Earth", "Outline: Natural Earth")
    )
}

/// Where a station's coordinates lie, against the country it is registered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lies {
    /// Inside, or within [`TOLERANCE`] of, the country's outline.
    Inside,
    /// Farther than that — and whether the swapped pair would be inside.
    Outside {
        /// Latitude and longitude, exchanged, land in the country.
        swapped: bool,
    },
    /// The outline does not hold that country, so nothing is said.
    Unknown,
}

/// How far outside its country's simplified outline a point may lie before
/// the card says so: a degree. The outline leaves out small islands and is
/// simplified by 0.08°; the defects worth a warning — latitude and longitude
/// swapped, a sign lost, a decimal point moved — are hundreds of kilometres
/// off, and a warning that is wrong once is ignored after (spec 022).
pub const TOLERANCE: i64 = 100;

/// Where `(lon, lat)` lies against `country`'s outline.
#[must_use]
pub fn lies(country: &str, lon: i64, lat: i64) -> Lies {
    let rings: Vec<&[(i16, i16)]> = RINGS
        .iter()
        .filter(|(code, _)| code.eq_ignore_ascii_case(country))
        .map(|(_, ring)| *ring)
        .collect();
    if rings.is_empty() {
        return Lies::Unknown;
    }
    let near = |lon: i64, lat: i64| {
        rings
            .iter()
            .any(|ring| inside(ring, lon, lat) || within(ring, lon, lat, TOLERANCE))
    };
    if near(lon, lat) {
        Lies::Inside
    } else {
        Lies::Outside {
            swapped: near(lat, lon),
        }
    }
}

/// Even-odd ray casting, in whole hundredths.
fn inside(ring: &[(i16, i16)], x: i64, y: i64) -> bool {
    let mut odd = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (xi, yi) = (i64::from(ring[i].0), i64::from(ring[i].1));
        let (xj, yj) = (i64::from(ring[j].0), i64::from(ring[j].1));
        if (yi > y) != (yj > y)
            && (x - xi) * (yj - yi) * (yj - yi).signum() < (xj - xi) * (y - yi) * (yj - yi).signum()
        {
            odd = !odd;
        }
        j = i;
    }
    odd
}

/// Whether a point lies within `limit` of a ring's edge.
fn within(ring: &[(i16, i16)], x: i64, y: i64, limit: i64) -> bool {
    let limit2 = i128::from(limit * limit);
    ring.windows(2).any(|pair| {
        let (ax, ay) = (i128::from(pair[0].0), i128::from(pair[0].1));
        let (bx, by) = (i128::from(pair[1].0), i128::from(pair[1].1));
        let (px, py) = (i128::from(x), i128::from(y));
        let (abx, aby) = (bx - ax, by - ay);
        let (apx, apy) = (px - ax, py - ay);
        let length2 = abx * abx + aby * aby;
        let dot = apx * abx + apy * aby;
        if dot <= 0 || length2 == 0 {
            apx * apx + apy * apy <= limit2
        } else if dot >= length2 {
            let (bpx, bpy) = (px - bx, py - by);
            bpx * bpx + bpy * bpy <= limit2
        } else {
            let cross = apx * aby - apy * abx;
            cross * cross <= limit2 * length2
        }
    })
}

/// The asset card's locator: the outline around the station and a pin.
#[must_use]
pub fn locator(lon: i64, lat: i64, lang: Lang) -> String {
    let Some(view) = View::fit(&[(lon, lat)], 300) else {
        return String::new();
    };
    let label = lang.pick("Lage der Station", "Where the station is");
    let (x, y) = (view.x(lon), view.y(lat));
    format!(
        "<figure class=\"map locator\">{open}<circle class=\"pin\" cx=\"{x}\" cy=\"{y}\" r=\"14\"/>\
         <circle class=\"pin-core\" cx=\"{x}\" cy=\"{y}\" r=\"5\"/></svg>\
         <figcaption><span class=\"credit\">{credit}</span></figcaption></figure>",
        open = view.open(label),
        credit = lang.pick("Umrisse: Natural Earth", "Outline: Natural Earth"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_become_whole_hundredths() {
        assert_eq!(hundredths("50.775346"), Some(5077));
        assert_eq!(
            hundredths("-8.611"),
            Some(-862),
            "rounded down, not toward zero"
        );
        assert_eq!(hundredths("north"), None);
    }

    #[test]
    fn a_station_in_its_country_is_inside() {
        // Aachen, Berlin, Munich, Hamburg.
        for (lon, lat) in [(608, 5077), (1338, 5253), (1157, 4813), (999, 5355)] {
            assert_eq!(lies("DE", lon, lat), Lies::Inside, "{lon},{lat}");
        }
        assert_eq!(lies("de", 1338, 5253), Lies::Inside, "case does not matter");
    }

    #[test]
    fn swapped_coordinates_are_named_as_swapped() {
        // Frankfurt is 8.68° E 50.11° N; entered the other way round it lies
        // in the Indian Ocean off Somalia.
        assert_eq!(lies("DE", 5011, 868), Lies::Outside { swapped: true });
        // A lost sign puts Lisbon in the sea south of Sardinia: outside, and
        // not a swap.
        assert_eq!(lies("PT", 914, 3872), Lies::Outside { swapped: false });
    }

    #[test]
    fn a_station_near_a_border_or_on_an_island_is_not_warned_about() {
        // Görlitz on the Neisse, Kehl across from Strasbourg, Sylt and
        // Helgoland, which the outline leaves out.
        for (lon, lat) in [(1499, 5115), (781, 4857), (831, 5490), (788, 5418)] {
            assert_eq!(lies("DE", lon, lat), Lies::Inside, "{lon},{lat}");
        }
    }

    #[test]
    fn a_country_the_outline_does_not_hold_is_not_judged() {
        assert_eq!(lies("US", -7400, 4070), Lies::Unknown);
    }

    #[test]
    fn sr_150_an_identity_in_the_map_is_escaped() {
        let dots = [Dot {
            identity: "CP-<script>".to_owned(),
            lon: 608,
            lat: 5077,
            presence: "online\" onload=\"x".to_owned(),
        }];
        let svg = fleet(&dots, &[], "", Lang::De);
        assert!(!svg.contains("<script>"), "{svg}");
        assert!(svg.contains("CP-&lt;script&gt;"));
        assert!(
            !svg.contains("onload"),
            "presence is a closed set of classes"
        );
    }

    #[test]
    fn sr_153_a_map_references_nothing_outside_the_desk() {
        let dots = [Dot {
            identity: "CP-1".to_owned(),
            lon: 608,
            lat: 5077,
            presence: "online".to_owned(),
        }];
        for svg in [
            fleet(&dots, &[], "", Lang::De),
            locator(608, 5077, Lang::En),
        ] {
            let outside = svg.replace("http://www.w3.org/2000/svg", "");
            assert!(!outside.contains("http"), "{svg}");
        }
    }

    #[test]
    fn a_ring_is_clipped_to_the_box_and_nothing_crosses_it() {
        let square = [(0, 0), (100, 0), (100, 100), (0, 100)];
        assert_eq!(
            clip(&square, (-10, -10, 200, 200)),
            square,
            "inside, unchanged"
        );
        assert!(
            clip(&square, (200, 200, 300, 300)).is_empty(),
            "outside, gone"
        );
        let half = clip(&square, (50, -10, 200, 200));
        assert!(half.iter().all(|p| p.0 >= 50), "{half:?}");
        assert!(
            half.contains(&(50, 0)) && half.contains(&(50, 100)),
            "{half:?}"
        );
    }

    #[test]
    fn a_map_of_germany_draws_germany_and_little_else() {
        let svg = locator(1000, 5100, Lang::De);
        assert!(svg.len() < 20_000, "{} bytes", svg.len());
        assert!(svg.contains("class=\"land\""));
    }
}
