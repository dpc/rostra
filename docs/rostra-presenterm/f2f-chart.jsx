import { useState } from "react";

const TAU = Math.PI * 2;
const CX = 400;
const CY = 340;

function generateGraph() {
  const you = { id: "you", label: "You", x: CX, y: CY, tier: 0, color: "#e8f554" };

  const t1Names = ["Alice", "Bob", "Carol", "Dave", "Eve"];
  const t1R = 120;
  const t1Angles = t1Names.map((_, i) => TAU * (i / t1Names.length) - Math.PI / 2);
  const follows = t1Names.map((name, i) => ({
    id: `t1-${i}`, label: name,
    x: CX + Math.cos(t1Angles[i]) * t1R,
    y: CY + Math.sin(t1Angles[i]) * t1R,
    tier: 1, color: "#6ee7b7",
  }));

  const fofNames = [
    ["Frank", "Grace"],
    ["Heidi", "Ivan"],
    ["Judy", "Karl"],
    ["Liam", "Mia"],
    ["Nina", "Oscar"],
  ];
  const t2 = [];
  follows.forEach((f, fi) => {
    const baseAngle = Math.atan2(f.y - CY, f.x - CX);
    fofNames[fi].forEach((name, j) => {
      const angle = baseAngle + (j === 0 ? -0.45 : 0.45);
      const r = 100;
      t2.push({
        id: `t2-${fi}-${j}`, label: name,
        x: f.x + Math.cos(angle) * r,
        y: f.y + Math.sin(angle) * r,
        tier: 2, color: "#7dd3fc", parent: f.id,
      });
    });
  });

  // Red isolated nodes — outside the trust graph
  const isolated = [
    { id: "spam", label: "Spammer", x: CX - 210, y: CY - 190, tier: -1, color: "#f87171" },
    { id: "influencer", label: "Influencer", x: CX + 210, y: CY - 190, tier: -1, color: "#f87171" },
    { id: "psyops", label: "PsyOps", x: CX + 210, y: CY + 190, tier: -1, color: "#f87171" },
  ];

  const nodes = [you, ...follows, ...t2, ...isolated];
  const edges = [
    ...follows.map((f) => ({ from: "you", to: f.id })),
    ...t2.map((n) => ({ from: n.parent, to: n.id })),

  ];

  const nodeMap = {};
  nodes.forEach((n) => (nodeMap[n.id] = n));
  return { nodes, edges, nodeMap };
}

export default function RostraGraph() {
  const [graph] = useState(generateGraph);
  const [hovered, setHovered] = useState(null);
  const { nodes, edges, nodeMap } = graph;

  const connectedSet = new Set();
  if (hovered) {
    connectedSet.add(hovered);
    edges.forEach((e) => {
      if (e.from === hovered) connectedSet.add(e.to);
      if (e.to === hovered) connectedSet.add(e.from);
    });
  }

  const tierMeta = {
    [-1]: { r: 26, fontSize: 10, fontWeight: 600 },
    0: { r: 38, fontSize: 15, fontWeight: 700 },
    1: { r: 28, fontSize: 12, fontWeight: 600 },
    2: { r: 22, fontSize: 10, fontWeight: 500 },
  };

  const pad = 70;
  const xs = nodes.map((n) => n.x);
  const ys = nodes.map((n) => n.y);
  const vx = Math.min(...xs) - pad;
  const vy = Math.min(...ys) - pad;
  const vw = Math.max(...xs) - vx + pad;
  const vh = Math.max(...ys) - vy + pad;

  return (
    <div style={{
      background: "#0a0a12", minHeight: "100vh", display: "flex",
      alignItems: "center", justifyContent: "center",
      fontFamily: "'JetBrains Mono','Fira Code',monospace", color: "#e2e8f0", padding: 24,
    }}>
      <svg viewBox={`${vx} ${vy} ${vw} ${vh}`} style={{ width: "100%", maxWidth: 720, height: "auto" }}>
        <circle cx={CX} cy={CY} r={220} fill="#7dd3fc08" stroke="#1a1a2e" strokeWidth={1} strokeDasharray="4 8" />
        <circle cx={CX} cy={CY} r={120} fill="#6ee7b710" stroke="#1a1a2e" strokeWidth={1} strokeDasharray="4 8" />

        {edges.map((e, i) => {
          const from = nodeMap[e.from];
          const to = nodeMap[e.to];
          if (!from || !to) return null;
          const active = !hovered || (connectedSet.has(e.from) && connectedSet.has(e.to));
          const dx = to.x - from.x;
          const dy = to.y - from.y;
          const len = Math.sqrt(dx * dx + dy * dy);
          const ux = dx / len;
          const uy = dy / len;
          const r1 = tierMeta[from.tier].r;
          const r2 = tierMeta[to.tier].r;
          const sx = from.x + ux * r1;
          const sy = from.y + uy * r1;
          const ex = to.x - ux * (r2 + 6);
          const ey = to.y - uy * (r2 + 6);
          const as = 7;
          const ax1 = ex - ux * as - uy * as * 0.45;
          const ay1 = ey - uy * as + ux * as * 0.45;
          const ax2 = ex - ux * as + uy * as * 0.45;
          const ay2 = ey - uy * as - ux * as * 0.45;

          return (
            <g key={i} opacity={active ? (e.cross ? 0.3 : 0.7) : 0.1}>
              <line x1={sx} y1={sy} x2={ex} y2={ey}
                stroke={e.cross ? "#475569" : from.color}
                strokeWidth={e.cross ? 1 : 1.5}
                strokeDasharray="none"
              />
              {!e.cross && (
                <polygon points={`${ex},${ey} ${ax1},${ay1} ${ax2},${ay2}`}
                  fill={from.color} opacity={active ? 0.8 : 0.15}
                />
              )}
            </g>
          );
        })}

        {nodes.map((n) => {
          const isIsolated = n.tier === -1;
          const active = isIsolated ? (hovered === n.id || !hovered) : (!hovered || connectedSet.has(n.id));
          const { r, fontSize, fontWeight } = tierMeta[n.tier];
          return (
            <g key={n.id}
              onPointerEnter={() => setHovered(n.id)}
              onPointerLeave={() => setHovered(null)}
              style={{ cursor: "pointer" }}
              opacity={active ? 1 : 0.15}
            >
              {hovered === n.id && (
                <circle cx={n.x} cy={n.y} r={r + 4} fill="none"
                  stroke={n.color} strokeWidth={2} opacity={0.4}
                />
              )}
              <circle cx={n.x} cy={n.y} r={r}
                fill={`${n.color}18`}
                stroke={n.color}
                strokeWidth={n.tier === 0 ? 2.5 : isIsolated ? 2 : 1.5}
                strokeDasharray="none"
              />
              <text x={n.x} y={n.y + 1}
                textAnchor="middle" dominantBaseline="central"
                fill={n.color} fontSize={fontSize} fontWeight={fontWeight}
                fontFamily="inherit"
              >
                {n.label}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
