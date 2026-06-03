import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, CartesianGrid,
  PieChart, Pie, Cell, Legend,
  LineChart, Line,
} from "recharts";

// ── Types ────────────────────────────────────────────────────────────────────

interface DailySummaryRow {
  date: string;
  app_id: string;
  app_name: string;
  total_secs: number;
  open_count: number;
}

interface HourlyUsageRow {
  hour: number;
  total_secs: number;
}

interface SessionRow {
  id: number;
  app_id: string;
  app_name: string;
  category: string;
  opened_at: string;
  closed_at: string | null;
  duration_secs: number | null;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

const fmtMins = (secs: number) => {
  const m = Math.floor(secs / 60);
  if (m === 0) return "0m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  const rem = m % 60;
  return rem === 0 ? `${h}h` : `${h}h ${rem}m`;
};

// For bar chart tooltip — show full readable label
const fmtBarTooltip = (secs: number) => {
  const m = Math.floor(secs / 60);
  if (m === 0) return "Less than a minute";
  if (m < 60) return `${m} minutes`;
  const h = Math.floor(m / 60);
  const rem = m % 60;
  return rem === 0 ? `${h} hour${h > 1 ? "s" : ""}` : `${h}h ${rem}m`;
};

const fmtDate = (d: string) => {
  const [, m, day] = d.split("-");
  const months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
  return `${months[parseInt(m) - 1]} ${parseInt(day)}`;
};

const COLORS = [
  "#5FA8D3", "#7EC8E3", "#3D7EAA", "#A8D8EA",
  "#F7B731", "#FC5C65", "#45AAB8", "#26C281",
  "#8854D0", "#FD9644",
];

const HOURS = Array.from({ length: 24 }, (_, i) =>
  i === 0 ? "12am" : i < 12 ? `${i}am` : i === 12 ? "12pm" : `${i - 12}pm`
);

// ── Subcomponents ────────────────────────────────────────────────────────────

function StatCard({ label, value, sub, color }: {
  label: string; value: string; sub?: string; color?: string;
}) {
  return (
    <div style={{
      background: "rgba(255,255,255,0.04)",
      border: "1px solid rgba(255,255,255,0.08)",
      borderRadius: 14,
      padding: "18px 22px",
      display: "flex",
      flexDirection: "column",
      gap: 6,
      minWidth: 140,
      flex: 1,
    }}>
      <div style={{ fontSize: 11, color: "#8A9BAD", textTransform: "uppercase", letterSpacing: 0.8 }}>{label}</div>
      <div style={{ fontSize: 26, fontWeight: 700, color: color ?? "#E0E8F0" }}>{value}</div>
      {sub && <div style={{ fontSize: 11, color: "#8A9BAD" }}>{sub}</div>}
    </div>
  );
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      fontSize: 13,
      fontWeight: 600,
      color: "#AAB8C5",
      textTransform: "uppercase",
      letterSpacing: 1,
      marginBottom: 14,
      marginTop: 28,
      display: "flex",
      alignItems: "center",
      gap: 8,
    }}>
      {children}
    </div>
  );
}

// ── Main Analytics Component ─────────────────────────────────────────────────

export default function Analytics({ onClose }: { onClose: () => void }) {
  const [range, setRange] = useState(1);
  const [dailyTotals, setDailyTotals] = useState<DailySummaryRow[]>([]);
  const [appTotals, setAppTotals] = useState<DailySummaryRow[]>([]);
  const [hourlyUsage, setHourlyUsage] = useState<HourlyUsageRow[]>([]);
  const [longSessions, setLongSessions] = useState<SessionRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [debugInfo, setDebugInfo] = useState<Record<string, unknown> | null>(null);

  useEffect(() => {
    fetchAll();
  }, [range]);

  const fetchAll = async () => {
    setLoading(true);
    try {
      // Single IPC call — one DB connection, all queries at once
      const payload = await invoke<{
        daily_totals: DailySummaryRow[];
        app_totals: DailySummaryRow[];
        hourly_usage: HourlyUsageRow[];
        long_sessions: SessionRow[];
      }>("get_analytics", { days: range });

      setDailyTotals(payload.daily_totals ?? []);
      setAppTotals(payload.app_totals ?? []);
      setHourlyUsage(payload.hourly_usage ?? []);
      setLongSessions(payload.long_sessions ?? []);
    } catch (err) {
      console.error("Analytics fetch error:", err);
    }
    setLoading(false);
  };

  // ── Derived data ────────────────────────────────────────────────

  // Daily bar chart — sum all apps per day
  const dailyBarData = Object.entries(
    dailyTotals.reduce((acc, r) => {
      acc[r.date] = (acc[r.date] ?? 0) + r.total_secs;
      return acc;
    }, {} as Record<string, number>)
  ).map(([date, total_secs]) => ({ date: fmtDate(date), total_secs, total_mins: Math.round(total_secs / 60) }));

  // Donut — top 8 apps
  const donutData = appTotals.slice(0, 8).map(r => ({
    name: r.app_name,
    value: Math.round(r.total_secs / 60),
  }));

  // Hourly heatmap data
  const hourlyMap: Record<number, number> = {};
  hourlyUsage.forEach(r => { hourlyMap[r.hour] = r.total_secs; });
  const maxHourly = Math.max(...Object.values(hourlyMap), 1);

  // Total screen time
  const totalSecs = appTotals.reduce((a, r) => a + r.total_secs, 0);
  const avgDailySecs = totalSecs / range;

  // Eye strain: sessions > 20min
  const eyeStrainCount = longSessions.length;
  const lateNightSecs = hourlyUsage
    .filter(r => r.hour >= 21 || r.hour < 6)
    .reduce((a, r) => a + r.total_secs, 0);

  // Break compliance (sessions under 20min / total)
  // Approximate from daily open_count and total sessions
  const totalOpens = appTotals.reduce((a, r) => a + r.open_count, 0);
  const compliancePct = totalOpens > 0
    ? Math.round(((totalOpens - eyeStrainCount) / totalOpens) * 100)
    : 100;

  // Eye strain score (0–100, lower is better)
  const eyeScore = Math.min(100, Math.round(
    (eyeStrainCount * 10) +
    (lateNightSecs / 3600) * 15
  ));

  // Line chart for session lengths
  const sessionLineData = longSessions.slice(0, 20).map(s => ({
    name: s.app_name,
    mins: Math.round((s.duration_secs ?? 0) / 60),
  }));

  return (
    <div style={{
      position: "fixed",
      inset: 0,
      background: "#1A2530",
      zIndex: 5000,
      display: "flex",
      flexDirection: "column",
      fontFamily: "'Segoe UI Variable', 'Segoe UI', system-ui, sans-serif",
      color: "#E0E8F0",
      overflowY: "auto",
    }}>
      {/* ── Header ── */}
      <div style={{
        padding: "24px 36px 0",
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        borderBottom: "1px solid rgba(255,255,255,0.06)",
        paddingBottom: 20,
        position: "sticky",
        top: 0,
        background: "#1A2530",
        zIndex: 10,
      }}>
        <div>
          <h1 style={{ margin: 0, fontSize: 24, fontWeight: 700 }}>📊 Analytics</h1>
          <p style={{ margin: "4px 0 0", color: "#8A9BAD", fontSize: 13 }}>
            Screen time & eye health insights
          </p>
        </div>

        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          {/* Range selector */}
          {[1, 7, 14, 30].map(d => (
            <button
              key={d}
              onClick={() => setRange(d)}
              style={{
                padding: "7px 16px",
                borderRadius: 8,
                border: "none",
                background: range === d ? "#5FA8D3" : "rgba(255,255,255,0.07)",
                color: range === d ? "#fff" : "#AAB8C5",
                cursor: "pointer",
                fontSize: 13,
                fontWeight: range === d ? 600 : 400,
                transition: "all 0.15s",
              }}
            >
              {d}d
            </button>
          ))}

          <button
            onClick={async () => {
              const result = await invoke<Record<string, unknown>>("debug_analytics");
              setDebugInfo(result);
              console.log("DEBUG ANALYTICS:", result);
            }}
            style={{
              padding: "7px 16px",
              borderRadius: 8,
              border: "1px solid rgba(247,183,49,0.4)",
              background: "rgba(247,183,49,0.1)",
              color: "#F7B731",
              cursor: "pointer",
              fontSize: 13,
            }}
          >
            🐛 Debug DB
          </button>

          <button
            onClick={onClose}
            style={{
              padding: "7px 16px",
              borderRadius: 8,
              border: "1px solid rgba(255,255,255,0.1)",
              background: "transparent",
              color: "#AAB8C5",
              cursor: "pointer",
              fontSize: 13,
              marginLeft: 8,
            }}
          >
            ✕ Close
          </button>
        </div>
      </div>

      {/* ── Body ── */}
      <div style={{ padding: "28px 36px", flex: 1 }}>
        {/* ── Debug Panel ── */}
        {debugInfo && (
          <div style={{
            background: "rgba(247,183,49,0.08)",
            border: "1px solid rgba(247,183,49,0.3)",
            borderRadius: 12,
            padding: 16,
            marginBottom: 24,
            fontSize: 12,
            fontFamily: "monospace",
            color: "#F7B731",
            whiteSpace: "pre-wrap",
            wordBreak: "break-all",
          }}>
            {JSON.stringify(debugInfo, null, 2)}
          </div>
        )}

        {loading ? (
          <div style={{ textAlign: "center", color: "#8A9BAD", marginTop: 80, fontSize: 14 }}>
            Loading analytics...
          </div>
        ) : totalSecs === 0 ? (
          <div style={{ textAlign: "center", color: "#8A9BAD", marginTop: 80 }}>
            <div style={{ fontSize: 48, marginBottom: 16 }}>📭</div>
            <div style={{ fontSize: 16, fontWeight: 600, color: "#C8D4DF" }}>No data yet</div>
            <div style={{ fontSize: 13, marginTop: 8 }}>
              Open some apps from the drawer to start tracking usage.
            </div>
          </div>
        ) : (
          <>
            {/* ── Stat Cards ── */}
            <SectionTitle>📈 Overview — {range === 1 ? "today" : `last ${range} days`}</SectionTitle>
            <div style={{ display: "flex", gap: 14, flexWrap: "wrap" }}>
              <StatCard
                label="Total Screen Time"
                value={fmtMins(totalSecs)}
                sub={`~${fmtMins(avgDailySecs)} / day avg`}
              />
              <StatCard
                label="Apps Used"
                value={String(appTotals.length)}
                sub={`${totalOpens} total opens`}
              />
              <StatCard
                label="Long Sessions"
                value={String(eyeStrainCount)}
                sub="Sessions over 20 min"
                color={eyeStrainCount > 5 ? "#FC5C65" : "#26C281"}
              />
              <StatCard
                label="Break Compliance"
                value={`${compliancePct}%`}
                sub="20-20-20 rule"
                color={compliancePct >= 70 ? "#26C281" : compliancePct >= 40 ? "#F7B731" : "#FC5C65"}
              />
              <StatCard
                label="Late Night Usage"
                value={fmtMins(lateNightSecs)}
                sub="After 9PM / before 6AM"
                color={lateNightSecs > 3600 ? "#FC5C65" : "#E0E8F0"}
              />
              <StatCard
                label="Eye Strain Score"
                value={`${eyeScore}/100`}
                sub="Lower is better"
                color={eyeScore < 30 ? "#26C281" : eyeScore < 60 ? "#F7B731" : "#FC5C65"}
              />
            </div>

            {/* ── Daily Bar Chart ── */}
            <SectionTitle>📅 Daily Screen Time</SectionTitle>
            <div style={{
              background: "rgba(255,255,255,0.03)",
              borderRadius: 14,
              border: "1px solid rgba(255,255,255,0.07)",
              padding: "20px 16px",
            }}>
              <ResponsiveContainer width="100%" height={220}>
                <BarChart data={dailyBarData} barSize={28}>
                  <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.05)" />
                  <XAxis dataKey="date" tick={{ fill: "#8A9BAD", fontSize: 11 }} />
                  <YAxis
                    tick={{ fill: "#8A9BAD", fontSize: 11 }}
                    tickFormatter={(v: number) => {
                      const m = Math.round(v);
                      if (m < 60) return `${m}m`;
                      return `${Math.floor(m / 60)}h${m % 60 > 0 ? ` ${m % 60}m` : ""}`;
                    }}
                  />
                  <Tooltip
                    contentStyle={{ background: "#1E2B38", border: "1px solid rgba(255,255,255,0.1)", borderRadius: 8, fontSize: 12 }}
                    formatter={(v: unknown) => [fmtBarTooltip((v as number) * 60), "Screen time"]}
                  />
                  <Bar dataKey="total_mins" fill="#5FA8D3" radius={[6, 6, 0, 0]} />
                </BarChart>
              </ResponsiveContainer>
            </div>

            {/* ── App Usage Donut ── */}
            <SectionTitle>🍩 App Usage Breakdown</SectionTitle>
            <div style={{
              background: "rgba(255,255,255,0.03)",
              borderRadius: 14,
              border: "1px solid rgba(255,255,255,0.07)",
              padding: "20px 16px",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
            }}>
              {donutData.length === 0 ? (
                <div style={{ color: "#8A9BAD", fontSize: 13, padding: 40 }}>No app data yet</div>
              ) : (
                <ResponsiveContainer width="100%" height={260}>
                  <PieChart>
                    <Pie
                      data={donutData}
                      cx="50%"
                      cy="50%"
                      innerRadius={70}
                      outerRadius={110}
                      paddingAngle={3}
                      dataKey="value"
                    >
                      {donutData.map((_, i) => (
                        <Cell key={i} fill={COLORS[i % COLORS.length]} />
                      ))}
                    </Pie>
                    <Tooltip
                      contentStyle={{ background: "#1E2B38", border: "1px solid rgba(255,255,255,0.1)", borderRadius: 8, fontSize: 12 }}
                      formatter={(v: unknown) => [`${Math.round(v as number)} min`, "Usage"]}
                    />
                    <Legend
                      formatter={(value) => <span style={{ color: "#C8D4DF", fontSize: 12 }}>{value}</span>}
                    />
                  </PieChart>
                </ResponsiveContainer>
              )}
            </div>

            {/* ── Hourly Heatmap ── */}
            <SectionTitle>🕐 Hourly Activity Heatmap</SectionTitle>
            <div style={{
              background: "rgba(255,255,255,0.03)",
              borderRadius: 14,
              border: "1px solid rgba(255,255,255,0.07)",
              padding: "20px 16px",
            }}>
              <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
                {Array.from({ length: 24 }, (_, hour) => {
                  const secs = hourlyMap[hour] ?? 0;
                  const intensity = secs / maxHourly;
                  const isLateNight = hour >= 21 || hour < 6;
                  return (
                    <div key={hour} title={`${HOURS[hour]}: ${fmtMins(secs)}`} style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 4 }}>
                      <div style={{
                        width: 36,
                        height: 36,
                        borderRadius: 8,
                        background: secs === 0
                          ? "rgba(255,255,255,0.04)"
                          : isLateNight
                          ? `rgba(252, 92, 101, ${0.2 + intensity * 0.8})`
                          : `rgba(95, 168, 211, ${0.2 + intensity * 0.8})`,
                        border: "1px solid rgba(255,255,255,0.06)",
                        cursor: "default",
                        transition: "transform 0.1s",
                      }}
                      onMouseEnter={e => (e.currentTarget.style.transform = "scale(1.15)")}
                      onMouseLeave={e => (e.currentTarget.style.transform = "scale(1)")}
                      />
                      <span style={{ fontSize: 9, color: "#8A9BAD" }}>{HOURS[hour]}</span>
                    </div>
                  );
                })}
              </div>
              <div style={{ display: "flex", gap: 16, marginTop: 14, fontSize: 11, color: "#8A9BAD" }}>
                <span><span style={{ color: "#5FA8D3" }}>■</span> Active hours</span>
                <span><span style={{ color: "#FC5C65" }}>■</span> Late night (eye strain risk)</span>
                <span>Hover each cell for details</span>
              </div>
            </div>

            {/* ── Long Sessions Line Chart ── */}
            {longSessions.length > 0 && (
              <>
                <SectionTitle>⚠️ Sessions Over 20 Minutes</SectionTitle>
                <div style={{
                  background: "rgba(255,255,255,0.03)",
                  borderRadius: 14,
                  border: "1px solid rgba(252,92,101,0.2)",
                  padding: "20px 16px",
                }}>
                  <div style={{ fontSize: 12, color: "#FC5C65", marginBottom: 14 }}>
                    ⚠ {eyeStrainCount} session{eyeStrainCount !== 1 ? "s" : ""} exceeded 20 minutes without a break — consider the 20-20-20 rule.
                  </div>
                  <ResponsiveContainer width="100%" height={180}>
                    <LineChart data={sessionLineData}>
                      <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.05)" />
                      <XAxis dataKey="name" tick={{ fill: "#8A9BAD", fontSize: 10 }} />
                      <YAxis
                    tick={{ fill: "#8A9BAD", fontSize: 11 }}
                    tickFormatter={(v: number) => {
                      const m = Math.round(v);
                      if (m < 60) return `${m}m`;
                      return `${Math.floor(m / 60)}h${m % 60 > 0 ? ` ${m % 60}m` : ""}`;
                    }}
                  />
                      <Tooltip
                        contentStyle={{ background: "#1E2B38", border: "1px solid rgba(255,255,255,0.1)", borderRadius: 8, fontSize: 12 }}
                        formatter={(v: unknown) => [`${Math.round(v as number)} min`, "Duration"]}
                      />
                      {/* 20-min threshold line */}
                      <Line type="monotone" dataKey="mins" stroke="#FC5C65" strokeWidth={2} dot={{ fill: "#FC5C65", r: 4 }} />
                    </LineChart>
                  </ResponsiveContainer>
                </div>
              </>
            )}

            {/* ── Top Apps Table ── */}
            <SectionTitle>🏆 Top Apps</SectionTitle>
            <div style={{
              background: "rgba(255,255,255,0.03)",
              borderRadius: 14,
              border: "1px solid rgba(255,255,255,0.07)",
              overflow: "hidden",
            }}>
              {appTotals.slice(0, 10).map((app, i) => {
                const pct = totalSecs > 0 ? (app.total_secs / totalSecs) * 100 : 0;
                return (
                  <div key={app.app_id} style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 14,
                    padding: "12px 20px",
                    borderBottom: i < appTotals.length - 1 ? "1px solid rgba(255,255,255,0.05)" : "none",
                  }}>
                    <span style={{ fontSize: 13, color: "#8A9BAD", width: 20, textAlign: "right" }}>
                      {i + 1}
                    </span>
                    <div style={{ flex: 1 }}>
                      <div style={{ fontSize: 13, fontWeight: 500, color: "#E0E8F0", marginBottom: 4 }}>
                        {app.app_name}
                      </div>
                      <div style={{ height: 4, background: "rgba(255,255,255,0.06)", borderRadius: 4, overflow: "hidden" }}>
                        <div style={{
                          height: "100%",
                          width: `${pct}%`,
                          background: COLORS[i % COLORS.length],
                          borderRadius: 4,
                          transition: "width 0.4s ease",
                        }} />
                      </div>
                    </div>
                    <div style={{ textAlign: "right", minWidth: 70 }}>
                      <div style={{ fontSize: 13, fontWeight: 600, color: "#E0E8F0" }}>{fmtMins(app.total_secs)}</div>
                      <div style={{ fontSize: 11, color: "#8A9BAD" }}>{app.open_count}x opened</div>
                    </div>
                  </div>
                );
              })}
            </div>

          </>
        )}
      </div>
    </div>
  );
}