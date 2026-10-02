/** An exact distance, which is two inequalities pointing opposite ways. */
export function fix(from, to, distance, placement) {
    return [
        { from, to, weight: distance, ...(placement ? { placement } : {}) },
        { from: to, to: from, weight: -distance, ...(placement ? { placement } : {}) },
    ];
}
/**
 * Solve, or report the placements that fight.
 *
 * Longest path from a virtual source joined to everything at zero, which is
 * Bellman-Ford with the comparison flipped. A distance that keeps growing after
 * one pass per node means the constraints run in a circle that demands ever
 * more room, and that circle is exactly the set of placements to quote back.
 */
export function tightest(count, constraints) {
    const positions = new Array(count).fill(0);
    const cameFrom = new Array(count).fill(undefined);
    for (let pass = 0; pass < count; pass += 1) {
        let moved = false;
        for (const constraint of constraints) {
            const candidate = positions[constraint.from] + constraint.weight;
            if (candidate > positions[constraint.to] + 1e-9) {
                positions[constraint.to] = candidate;
                cameFrom[constraint.to] = constraint;
                moved = true;
            }
        }
        if (!moved)
            return { positions };
    }
    // Still moving after a full pass per node, so some loop demands more room
    // every time round it. Walk backwards to find it.
    for (const constraint of constraints) {
        if (positions[constraint.from] + constraint.weight > positions[constraint.to] + 1e-9) {
            return { contradiction: { placements: loopFrom(constraint.to, cameFrom, count) } };
        }
    }
    return { contradiction: { placements: [] } };
}
/**
 * Which members can be driven apart from which, along one axis.
 *
 * A constraint says `pos[to] >= pos[from] + weight`, so it bounds `to` from
 * below and leaves it free to move further away. Follow those edges and the
 * question "may this one end up beyond that one?" becomes plain reachability:
 * if `j` is reachable from `i` and `i` is not reachable from `j`, then the file
 * lets the distance from `i` to `j` grow without limit and never lets it be
 * closed from the other side. Pushing `j` past `i` is then the one separation
 * the file allows, and no choice was made.
 *
 * Reachable both ways means the two are pinned at a fixed distance, so they
 * cannot be separated along this axis at all. Reachable neither way means the
 * file said nothing that orders them, which is the case the caller must refuse
 * rather than guess at.
 */
export function reachability(count, constraints) {
    const reach = Array.from({ length: count }, () => new Array(count).fill(false));
    for (const constraint of constraints)
        reach[constraint.from][constraint.to] = true;
    for (let via = 0; via < count; via += 1) {
        for (let from = 0; from < count; from += 1) {
            if (!reach[from][via])
                continue;
            for (let to = 0; to < count; to += 1) {
                if (reach[via][to])
                    reach[from][to] = true;
            }
        }
    }
    return reach;
}
/** The placements on the loop reached by following each position back to what set it. */
function loopFrom(start, cameFrom, count) {
    let at = start;
    for (let step = 0; step < count; step += 1) {
        const previous = cameFrom[at];
        if (previous === undefined)
            break;
        at = previous.from;
    }
    const placements = [];
    const seen = new Set();
    let cursor = at;
    while (!seen.has(cursor)) {
        seen.add(cursor);
        const previous = cameFrom[cursor];
        if (previous === undefined)
            break;
        if (previous.placement)
            placements.push(previous.placement);
        cursor = previous.from;
    }
    return placements.reverse();
}
