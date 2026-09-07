/// A media item size, in pixels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PhysicalSize {
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// The position and size of a media item within the album grid, in pixels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// The width of a tile relative to its height. Individual ratios are clamped
/// between these bounds, so that extremely wide or tall media cannot distort
/// the whole grid.
const MIN_RATIO: f64 = 0.5;
const MAX_RATIO: f64 = 2.0;

/// Computes the grid of an album from the sizes of its items.
///
/// The items are distributed over rows according to the number of items and
/// their aspect ratios (see `row_splits`), and every row is laid out so that
/// its tiles are as tall as the row is high: the row height is the width
/// available after the spacing divided by the sum of the tile ratios. The
/// returned rects start at (0, 0) and span at most `max_width` horizontally.
pub(crate) fn calculate_album_grid(
    sizes: &[PhysicalSize],
    max_width: f64,
    spacing: f64,
) -> Vec<Rect> {
    let n = sizes.len();
    if n == 0 {
        return Vec::new();
    }

    let ratios: Vec<f64> = sizes
        .iter()
        .map(|size| (size.width / size.height).clamp(MIN_RATIO, MAX_RATIO))
        .collect();

    let splits = row_splits(n, &ratios);

    let mut rects = Vec::with_capacity(n);
    let mut item_idx = 0;
    let mut current_y = 0.0_f64;

    for row_size in splits {
        // Guard against a split asking for more items than are left, so that
        // the slicing below can never panic.
        let row_size = row_size.min(n - item_idx);
        if row_size == 0 {
            break;
        }

        let row_ratios = &ratios[item_idx..item_idx + row_size];
        let ratio_sum: f64 = row_ratios.iter().sum();

        let available_w = (max_width - (row_size as f64 - 1.0) * spacing).max(0.0);
        let row_height = available_w / ratio_sum;

        let mut current_x = 0.0_f64;
        for &r in row_ratios {
            let item_w = r * row_height;
            rects.push(Rect {
                x: current_x.round(),
                y: current_y.round(),
                width: item_w.round(),
                height: row_height.round(),
            });
            current_x += item_w + spacing;
        }

        current_y += row_height + spacing;
        item_idx += row_size;

        if item_idx >= n {
            break;
        }
    }

    rects
}

/// Selects the row split pattern of an album, i.e. the number of items of
/// every row, based on the item count and their aspect ratios.
///
/// Two portrait items share a single row, while other pairs are stacked.
/// Three items are laid out with the oldest one as a full-width tile on top
/// of a row of two. Larger albums are grouped in rows of three, with the
/// remainder distributed over the last rows.
fn row_splits(n: usize, ratios: &[f64]) -> Vec<usize> {
    match n {
        1 => vec![1],
        2 => {
            if ratios[0] < 0.9 && ratios[1] < 0.9 {
                vec![2]
            } else {
                vec![1, 1]
            }
        }
        3 => vec![1, 2],
        4 => vec![2, 2],
        5 => vec![2, 3],
        6 => vec![3, 3],
        7 => vec![3, 2, 2],
        8 => vec![3, 3, 2],
        9 => vec![3, 3, 3],
        10 => vec![4, 3, 3],
        _ => {
            let mut splits = vec![3; n / 3];
            match n % 3 {
                0 => {}
                1 => {
                    if let Some(last) = splits.last_mut() {
                        *last += 1;
                    } else {
                        splits.push(1);
                    }
                }
                _ => splits.push(2),
            }
            splits
        }
    }
}
