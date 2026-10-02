/**
 * Finding a line's way through the open space between boxes.
 *
 * Every box is a wall. A line may turn only where two tracks cross, and the
 * tracks are few: a line a little way out from each side of each wall, one
 * down the middle of each gap between walls, and the lines through the two
 * ends. That is the whole of the open space a right-angled line ever needs,
 * reduced to a grid of a few hundred points for a diagram of ordinary size.
 *
 * The line leaves its start heading straight out and arrives at its end heading
 * straight in, with a straight stretch at each end at least `stub` long, so an
 * arrowhead always lands on a straight piece. Between those two stretches it
 * takes the cheapest way through the grid, where cost is length plus a charge
 * for every turn and for every line already placed that it crosses. On a
 * right-angled grid many ways tie on length — a staircase is exactly as long as
 * one bend — and the turn charge breaks those ties toward the fewest bends,
 * which is the line a person would draw. What still ties after that goes over
 * the top rather than under, and round the right rather than the left.
 *
 * A way through a box is not a losing option here. It is not a way at all.
 */
/**
 * The cheapest right-angled line from `start` to `end` that enters no wall, or
 * undefined when there is none.
 */
export function searchRoute(start, end, walls, options) {
    const { clear, turn } = options;
    // The stretches at the ends are fixed. Each crosses its own box's margin and
    // runs `stub` further, or, where another box is closer than that, stops in
    // the middle of the gap between the two margins: a node placed a `tight` gap
    // away still leaves room to get out.
    const others = (own) => walls.filter((wall) => !own.includes(wall));
    const reach = (from, own) => {
        const exit = Math.max(0, ...own.map((wall) => depth(from, wall)));
        const wanted = Math.max(options.stub, exit + 1);
        const hit = Math.min(Infinity, ...others(own).map((wall) => distanceTo(from, wall, wanted)));
        const length = hit >= wanted ? wanted : (exit + hit) / 2;
        if (length <= exit)
            return undefined;
        return { x: snap(from.x + from.dx * length), y: snap(from.y + from.dy * length) };
    };
    const out = reach(start, options.ownStart);
    const back = reach(end, options.ownEnd);
    if (!out || !back)
        return undefined;
    if (inside(out, walls) || inside(back, walls))
        return undefined;
    // No way between the two is shorter than straight across and straight down.
    const limit = options.limit ?? Infinity;
    if (Math.abs(back.x - out.x) + Math.abs(back.y - out.y) - 1e-6 >= limit)
        return undefined;
    const bounds = options.bounds;
    const gate = options.gate;
    // How far a stretch keeps from a placed one beside it: a lane, or clear of
    // the text either of them carries there.
    const halves = options.takenHalves ?? [];
    const ownHalf = options.ownHalf ?? 0;
    const inGate = (a, b) => {
        if (!gate)
            return false;
        const along = gate.run === 'x' ? Math.abs(a.y - b.y) < 0.5 : Math.abs(a.x - b.x) < 0.5;
        const level = gate.run === 'x' ? a.y : a.x;
        return along && level > gate.lo && level < gate.hi;
    };
    const spacing = (line, stretch, own) => Math.max(options.lane, (halves[line]?.[stretch] ?? 0) + own);
    const gateTracks = (axis) => !gate ? [] : axis === gate.run ? [gate.at] : [(gate.lo + gate.hi) / 2];
    const xs = tracks(walls, 'x', clear, [out.x, back.x, ...gateTracks('x')], options.taken, spacing, ownHalf)
        .filter((x) => !bounds || (x >= bounds.minX && x <= bounds.maxX) || x === out.x || x === back.x);
    const ys = tracks(walls, 'y', clear, [out.y, back.y, ...gateTracks('y')], options.taken, spacing, ownHalf)
        .filter((y) => !bounds || (y >= bounds.minY && y <= bounds.maxY) || y === out.y || y === back.y);
    const ix = new Map(xs.map((x, index) => [x, index]));
    const iy = new Map(ys.map((y, index) => [y, index]));
    const width = xs.length;
    const height = ys.length;
    const steps = stepTable(xs, ys, walls, clear, options.taken, spacing, ownHalf, (a, b) => (inGate(a, b) ? ownHalf : 0));
    const open = (i, j) => steps.open[j * width + i] === 1;
    // Four headings: 0 right, 1 down, 2 left, 3 up.
    const STEP = [
        [1, 0],
        [0, 1],
        [-1, 0],
        [0, -1],
    ];
    const heading = (dx, dy) => (dx > 0 ? 0 : dy > 0 ? 1 : dx < 0 ? 2 : 3);
    const first = heading(start.dx, start.dy);
    // The last stretch travels into the end, against the way its side faces.
    const last = heading(-end.dx, -end.dy);
    const si = ix.get(out.x);
    const sj = iy.get(out.y);
    const ti = ix.get(back.x);
    const tj = iy.get(back.y);
    // Cost is compared first on length and turns, then on a tie-break that
    // prefers a stretch higher up the page, or further right, so that of two
    // ways round that are exactly as long the line goes over the top. A state
    // also records whether the line has passed its gate yet; it may only finish
    // once it has.
    const size = width * height * 8;
    const cost = new Float64Array(size).fill(Infinity);
    const tie = new Float64Array(size).fill(Infinity);
    const from = new Int32Array(size).fill(-1);
    const state = (i, j, h, p) => ((j * width + i) * 4 + h) * 2 + p;
    const done = gate ? 1 : 0;
    // The search reaches out toward the end first: a state waits by what it has
    // cost so far plus the least the rest could cost, straight across and
    // straight down to the end. It does not settle each state once, since a
    // way just as long can arrive later with a better tie-break; it keeps on
    // until nothing waiting could come in as cheap as the end already has, and
    // what it finds is exactly what searching every direction alike would.
    // Until the line has passed its gate, that is by way of the gate.
    const toGo = (current) => {
        const cell = (current - (current % 8)) / 8;
        const i = cell % width;
        const point = { x: xs[i], y: ys[(cell - i) / width] };
        return leastLength(point, back, current % 2 === 1 ? undefined : gate);
    };
    const heap = new MinHeap();
    const begin = state(si, sj, first, 0);
    const goal = state(ti, tj, last, done);
    cost[begin] = 0;
    tie[begin] = 0;
    heap.push(begin, toGo(begin), 0);
    while (heap.size > 0) {
        const [current, f, t] = heap.pop();
        const c = cost[current];
        const due = c + toGo(current);
        if (f > due || (f === due && t > tie[current]))
            continue;
        // Everything still waiting costs at least this much.
        if (f >= limit || f > cost[goal] + 1e-9)
            break;
        const p = current % 2;
        const rest = (current - p) / 2;
        const h = rest % 4;
        const cell = (rest - h) / 4;
        const i = cell % width;
        const j = (cell - i) / width;
        if (current === goal)
            continue;
        for (let next = 0; next < 4; next += 1) {
            // A line never doubles straight back on itself.
            if ((next + 2) % 4 === h)
                continue;
            if (next !== h) {
                // Turning on the spot costs a turn and moves nowhere.
                relax(current, state(i, j, next, p), c + turn, t);
                continue;
            }
            const ni = i + STEP[next][0];
            const nj = j + STEP[next][1];
            if (ni < 0 || nj < 0 || ni >= width || nj >= height || !open(ni, nj))
                continue;
            // A step is filed under the point it starts from on its way right or down.
            const axis = next % 2;
            const key = next < 2 ? j * width + i : nj * width + ni;
            if (steps.closed[axis][key] === 1)
                continue;
            const a = { x: xs[i], y: ys[j] };
            const b = { x: xs[ni], y: ys[nj] };
            // Passing the gate: a stretch through it, reaching the line at `at`.
            const through = inGate(a, b) &&
                Math.min(gate.run === 'x' ? a.x : a.y, gate.run === 'x' ? b.x : b.y) <= gate.at + 0.01 &&
                Math.max(gate.run === 'x' ? a.x : a.y, gate.run === 'x' ? b.x : b.y) >= gate.at - 0.01;
            const np = p === 1 || through ? 1 : 0;
            // Running closer to a wall than `clear` counts double: the line may pass
            // a narrow gap, but where there is room it keeps its distance. Crossing a
            // line already placed costs what a turn does, so of two ways much the
            // same length the one that crosses nothing wins.
            const length = Math.abs(b.x - a.x) + Math.abs(b.y - a.y) + steps.crowd[axis][key] +
                turn * steps.cross[axis][key];
            // Higher up for a stretch across, further right for one down: a smaller
            // tie-break is preferred, so the across level counts as it is and the
            // down level counts negated.
            const lean = next % 2 === 0 ? a.y * length : -a.x * length;
            relax(current, state(ni, nj, next, np), c + length, t + lean);
        }
    }
    if (!(cost[goal] < limit))
        return undefined;
    function relax(previous, next, c, t) {
        if (c < cost[next] - 1e-9 || (Math.abs(c - cost[next]) <= 1e-9 && t < tie[next])) {
            cost[next] = c;
            tie[next] = t;
            from[next] = previous;
            heap.push(next, c + toGo(next), t);
        }
    }
    const path = [];
    for (let at = goal; at >= 0; at = from[at]) {
        const rest = (at - (at % 2)) / 2;
        const cell = (rest - (rest % 4)) / 4;
        const i = cell % width;
        const j = (cell - i) / width;
        const point = { x: xs[i], y: ys[j] };
        const previous = path[path.length - 1];
        if (!previous || previous.x !== point.x || previous.y !== point.y)
            path.push(point);
    }
    path.reverse();
    return { points: straightRuns([start, ...path, end]), cost: cost[goal] };
}
/**
 * The shortest a right-angled line from `a` to `b` could be: straight across
 * and straight down, or with a gate, across to its line at `at` and to a
 * level within it on the way.
 */
function leastLength(a, b, gate) {
    if (!gate)
        return Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
    const [u, v, bu, bv] = gate.run === 'x' ? [a.x, a.y, b.x, b.y] : [a.y, a.x, b.y, b.x];
    const off = Math.max(0, gate.lo - Math.max(v, bv), Math.min(v, bv) - gate.hi);
    return Math.abs(u - gate.at) + Math.abs(gate.at - bu) + Math.abs(v - bv) + 2 * off;
}
/**
 * The tracks on one axis: beside each wall at `clear` where that is open,
 * down the middle of every gap between walls, through the ends, and a lane
 * either side of each line already placed — a wider one where a text rides,
 * and wider again by `own` for this line's own text.
 */
function tracks(walls, axis, clear, ends, taken, spacing, own) {
    const lo = (wall) => (axis === 'x' ? wall.minX : wall.minY);
    const hi = (wall) => (axis === 'x' ? wall.maxX : wall.maxY);
    // The other axis, to tell a real gap — two walls facing each other across
    // it — from two edges that merely happen to lie near each other on this one.
    const olo = (wall) => (axis === 'x' ? wall.minY : wall.minX);
    const ohi = (wall) => (axis === 'x' ? wall.maxY : wall.maxX);
    const found = new Set(ends);
    for (const wall of walls) {
        found.add(lo(wall) - clear);
        found.add(hi(wall) + clear);
    }
    for (const a of walls) {
        for (const b of walls) {
            if (hi(a) < lo(b) && olo(a) < ohi(b) && olo(b) < ohi(a))
                found.add((hi(a) + lo(b)) / 2);
        }
    }
    taken.forEach((line, which) => {
        for (let index = 0; index + 1 < line.length; index += 1) {
            const [a, b] = [line[index], line[index + 1]];
            // A stretch across sits at a level on y; one down, at a level on x.
            const across = Math.abs(a.y - b.y) < 0.5;
            if ((axis === 'y') !== across)
                continue;
            // A lane either side of a placed line. One that runs close beside a box
            // is paid for as crowding, like any other track.
            const level = axis === 'y' ? a.y : a.x;
            for (const by of new Set([spacing(which, index, 0), spacing(which, index, own)])) {
                found.add(level - by);
                found.add(level + by);
            }
        }
    });
    return [...new Set([...found].map(snap))].sort((p, q) => p - q);
}
/** Coordinates to a hundredth, so a track and the end it runs through compare equal. */
function snap(value) {
    return Math.round(value * 100) / 100;
}
/** How far along its heading an end runs before it leaves a wall it starts inside; 0 if it is outside. */
function depth(from, wall) {
    if (!(from.x >= wall.minX && from.x <= wall.maxX && from.y >= wall.minY && from.y <= wall.maxY))
        return 0;
    if (from.dx > 0)
        return wall.maxX - from.x;
    if (from.dx < 0)
        return from.x - wall.minX;
    if (from.dy > 0)
        return wall.maxY - from.y;
    return from.y - wall.minY;
}
/**
 * How far along its heading an end runs before it meets a wall it is outside
 * of, looking no further than `limit`; Infinity if it meets none.
 */
function distanceTo(from, wall, limit) {
    const tip = { x: from.x + from.dx * limit, y: from.y + from.dy * limit };
    if (!blocked(from, tip, [wall]))
        return Infinity;
    if (from.dx > 0)
        return wall.minX - from.x;
    if (from.dx < 0)
        return from.x - wall.maxX;
    if (from.dy > 0)
        return wall.minY - from.y;
    return from.y - wall.maxY;
}
/** Whether a point lies strictly inside any wall. */
function inside(point, walls) {
    return walls.some((wall) => point.x > wall.minX && point.x < wall.maxX && point.y > wall.minY && point.y < wall.maxY);
}
/** Whether a level or upright stretch passes through the inside of any wall. */
function blocked(a, b, walls) {
    return walls.some((wall) => blocks(a, b, wall));
}
/** Whether a level or upright stretch passes through the inside of one wall. */
function blocks(a, b, wall) {
    const minX = Math.min(a.x, b.x);
    const maxX = Math.max(a.x, b.x);
    const minY = Math.min(a.y, b.y);
    const maxY = Math.max(a.y, b.y);
    if (minY === maxY)
        return minY > wall.minY && minY < wall.maxY && maxX > wall.minX && minX < wall.maxX;
    return minX > wall.minX && minX < wall.maxX && maxY > wall.minY && minY < wall.maxY;
}
/**
 * Every single step through the grid — from a point to its neighbor on the
 * right, or the one below — and what it costs beyond its length: whether it is
 * closed, by a wall or by running along a placed line; how far it runs close
 * beside a wall; how many placed lines it crosses. The search takes each step
 * many times over, once for each heading it arrives with, so these are worked
 * out here once, and each wall and each placed stretch is tested only against
 * the steps that lie near it. A step is filed at the index of the point it
 * starts from: `[0]` the step right, `[1]` the step down.
 */
function stepTable(xs, ys, walls, clear, taken, spacing, widest, ownAt) {
    const width = xs.length;
    const height = ys.length;
    const cells = width * height;
    const open = new Uint8Array(cells).fill(1);
    const closed = [new Uint8Array(cells), new Uint8Array(cells)];
    const crowd = [new Float64Array(cells), new Float64Array(cells)];
    const cross = [new Int32Array(cells), new Int32Array(cells)];
    // The indices of the tracks lying within `lo`..`hi`, with one to spare either
    // side: a range to test exactly, never a test itself.
    const near = (values, lo, hi) => [Math.max(0, firstAtLeast(values, lo) - 1), Math.min(values.length - 1, firstAtLeast(values, hi) + 1)];
    const each = ([i0, i1], [j0, j1], visit) => {
        for (let j = j0; j <= j1; j += 1) {
            for (let i = i0; i <= i1; i += 1) {
                const a = { x: xs[i], y: ys[j] };
                if (i + 1 < width)
                    visit(0, a, { x: xs[i + 1], y: ys[j] }, j * width + i);
                if (j + 1 < height)
                    visit(1, a, { x: xs[i], y: ys[j + 1] }, j * width + i);
            }
        }
    };
    for (const wall of walls) {
        const [i0, i1] = near(xs, wall.minX, wall.maxX);
        const [j0, j1] = near(ys, wall.minY, wall.maxY);
        for (let j = j0; j <= j1; j += 1) {
            for (let i = i0; i <= i1; i += 1) {
                const x = xs[i];
                const y = ys[j];
                if (x > wall.minX && x < wall.maxX && y > wall.minY && y < wall.maxY)
                    open[j * width + i] = 0;
            }
        }
        each([i0, i1], [j0, j1], (axis, a, b, key) => {
            if (blocks(a, b, wall))
                closed[axis][key] = 1;
        });
        each(near(xs, wall.minX - clear, wall.maxX + clear), near(ys, wall.minY - clear, wall.maxY + clear), (axis, a, b, key) => {
            crowd[axis][key] = Math.max(crowd[axis][key], crowding(a, b, wall, clear));
        });
    }
    taken.forEach((line, which) => {
        for (let index = 0; index + 1 < line.length; index += 1) {
            const [p, q] = [line[index], line[index + 1]];
            const reach = spacing(which, index, widest);
            const xr = near(xs, Math.min(p.x, q.x) - reach, Math.max(p.x, q.x) + reach);
            const yr = near(ys, Math.min(p.y, q.y) - reach, Math.max(p.y, q.y) + reach);
            each(xr, yr, (axis, a, b, key) => {
                if (runsAlong(a, b, p, q, spacing(which, index, ownAt(a, b))))
                    closed[axis][key] = 1;
                if (crosses(a, b, p, q))
                    cross[axis][key] = cross[axis][key] + 1;
            });
        }
    });
    return { open, closed, crowd, cross };
}
/** The index of the first of the sorted values at least `value`, or their count if none is. */
function firstAtLeast(values, value) {
    let lo = 0;
    let hi = values.length;
    while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (values[mid] < value)
            lo = mid + 1;
        else
            hi = mid;
    }
    return lo;
}
/** How much of a stretch runs within `clear` of one wall's side, alongside it; 0 if none does. */
function crowding(a, b, wall, clear) {
    const across = Math.abs(a.y - b.y) < 0.5;
    const level = across ? a.y : a.x;
    const from = across ? Math.min(a.x, b.x) : Math.min(a.y, b.y);
    const to = across ? Math.max(a.x, b.x) : Math.max(a.y, b.y);
    const [lo, hi] = across ? [wall.minY, wall.maxY] : [wall.minX, wall.maxX];
    const [alo, ahi] = across ? [wall.minX, wall.maxX] : [wall.minY, wall.maxY];
    const near = (level > lo - clear + 0.01 && level <= lo) || (level >= hi && level < hi + clear - 0.01);
    return near ? Math.max(0, Math.min(to, ahi) - Math.max(from, alo)) : 0;
}
/**
 * Whether a stretch runs along one stretch of a placed line, for any distance,
 * closer than `spacing`: a lane, or clear of a text either carries.
 */
function runsAlong(a, b, p, q, spacing) {
    const across = Math.abs(a.y - b.y) < 0.5;
    if ((Math.abs(p.y - q.y) < 0.5) !== across)
        return false;
    const level = across ? a.y : a.x;
    const other = across ? p.y : p.x;
    if (Math.abs(other - level) >= spacing - 0.5)
        return false;
    const lo = across ? Math.min(a.x, b.x) : Math.min(a.y, b.y);
    const hi = across ? Math.max(a.x, b.x) : Math.max(a.y, b.y);
    const pLo = across ? Math.min(p.x, q.x) : Math.min(p.y, q.y);
    const pHi = across ? Math.max(p.x, q.x) : Math.max(p.y, q.y);
    return Math.min(hi, pHi) - Math.max(lo, pLo) > 0.5;
}
/** Whether a stretch crosses one stretch of a placed line square on, touching neither end of either. */
function crosses(a, b, p, q) {
    const across = Math.abs(a.y - b.y) < 0.5;
    if ((Math.abs(p.y - q.y) < 0.5) === across)
        return false;
    const level = across ? a.y : a.x;
    const lo = across ? Math.min(a.x, b.x) : Math.min(a.y, b.y);
    const hi = across ? Math.max(a.x, b.x) : Math.max(a.y, b.y);
    const at = across ? p.x : p.y;
    const pLo = across ? Math.min(p.y, q.y) : Math.min(p.x, q.x);
    const pHi = across ? Math.max(p.y, q.y) : Math.max(p.x, q.x);
    return at > lo + 0.5 && at < hi - 0.5 && level > pLo + 0.5 && level < pHi - 0.5;
}
/** The points with every one dropped that sits on a straight run between its neighbors. */
function straightRuns(points) {
    const kept = [];
    for (const point of points) {
        const previous = kept[kept.length - 1];
        if (previous && Math.abs(previous.x - point.x) < 0.01 && Math.abs(previous.y - point.y) < 0.01)
            continue;
        kept.push(point);
        while (kept.length >= 3) {
            const [a, b, c] = kept.slice(-3);
            const level = Math.abs(a.y - b.y) < 0.01 && Math.abs(b.y - c.y) < 0.01;
            const upright = Math.abs(a.x - b.x) < 0.01 && Math.abs(b.x - c.x) < 0.01;
            if (!level && !upright)
                break;
            kept.splice(kept.length - 2, 1);
        }
    }
    return kept;
}
/** A binary heap of states, cheapest first, ties broken by the second cost. */
class MinHeap {
    items = [];
    get size() {
        return this.items.length;
    }
    push(state, cost, tie) {
        const items = this.items;
        items.push([state, cost, tie]);
        let at = items.length - 1;
        while (at > 0) {
            const parent = (at - 1) >> 1;
            if (!before(items[at], items[parent]))
                break;
            [items[at], items[parent]] = [items[parent], items[at]];
            at = parent;
        }
    }
    pop() {
        const items = this.items;
        const top = items[0];
        const tail = items.pop();
        if (items.length > 0) {
            items[0] = tail;
            let at = 0;
            for (;;) {
                const left = at * 2 + 1;
                const right = left + 1;
                let best = at;
                if (left < items.length && before(items[left], items[best]))
                    best = left;
                if (right < items.length && before(items[right], items[best]))
                    best = right;
                if (best === at)
                    break;
                [items[at], items[best]] = [items[best], items[at]];
                at = best;
            }
        }
        return top;
    }
}
function before(p, q) {
    return p[1] < q[1] - 1e-9 || (Math.abs(p[1] - q[1]) <= 1e-9 && p[2] < q[2]);
}
