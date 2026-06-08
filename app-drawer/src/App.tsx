import { useEffect, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import Analytics from "./Analytics";
import VoiceCommand from "./VoiceCommand";

interface AppItem {
  name: string;
  app_id: string;
  category: string;
  icon_base64?: string;
}

interface ContextMenu {
  x: number;
  y: number;
  app: AppItem;
  showMoveTo: boolean;
}

export default function App() {
  const [apps, setApps] = useState<AppItem[]>([]);
  const [categories, setCategories] = useState<string[]>([]);
  const [selectedCategory, setSelectedCategory] = useState<string>("Browser");
  const [newCategoryName, setNewCategoryName] = useState("");
  const [showAddCategory, setShowAddCategory] = useState(false);
  const [selectedApp, setSelectedApp] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [isSearchFocused, setIsSearchFocused] = useState(false);
  const [contextMenu, setContextMenu] = useState<ContextMenu | null>(null);
  const [dragOverCategory, setDragOverCategory] = useState<string | null>(null);
  const [draggingApp, setDraggingApp] = useState<AppItem | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const contextMenuRef = useRef<HTMLDivElement>(null);
  const draggingAppRef = useRef<AppItem | null>(null);
  const selectedAppRef = useRef<string | null>(null);
  const appsRef = useRef<AppItem[]>([]);
  const filteredAppsRef = useRef<AppItem[]>([]);
  const GRID_COLS = 8; // approximate columns matching minmax(80px) grid
  const [toast, setToast] = useState<{ msg: string; ok: boolean } | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [movingApp, setMovingApp] = useState<{ name: string; to: string } | null>(null);
  const [showAnalytics, setShowAnalytics] = useState(false);
  const [showVoice, setShowVoice] = useState(false);
  const activeSessionRef = useRef<{ id: number; app: AppItem } | null>(null);

  const showToast = (msg: string, ok = true) => {
    setToast({ msg, ok });
    setTimeout(() => setToast(null), 3000);
  };

  const safeInvoke = async <T = void,>(cmd: string, args?: Record<string, unknown>): Promise<{ ok: true; data: T } | { ok: false }> => {
    try {
      const data = await invoke<T>(cmd, args);
      return { ok: true, data };
    } catch (err) {
      const msg = typeof err === "string" ? err : JSON.stringify(err);
      showToast(`[${cmd}] ${msg}`, false);
      console.error(`[${cmd}]`, args, "->", err);
      return { ok: false };
    }
  };

  useEffect(() => {
    loadApps();
    loadCategories();

    // Listen for real-time icon updates from the background thread
    const unlisten = listen<{ app_id: string; icon_base64: string }>("icon-ready", (event) => {
      const { app_id, icon_base64 } = event.payload;
      setApps((prev) =>
        prev.map((app) =>
          app.app_id === app_id ? { ...app, icon_base64 } : app
        )
      );
    });

    // Open analytics directly when triggered from tray menu
    const unlistenAnalytics = listen("open-analytics", () => {
      setShowAnalytics(true);
    });

    // Reload apps when background scan completes
    const unlistenAppsReady = listen("apps-ready", () => {
      loadApps();
    });

    // Open voice command from tray or Ctrl+Shift+V
    const unlistenVoice = listen("open-voice", () => {
      setShowVoice(true);
    });

    return () => {
      unlisten.then((f) => f());
      unlistenAnalytics.then((f) => f());
      unlistenAppsReady.then((f) => f());
      unlistenVoice.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const handleClick = () => setContextMenu(null);
    const handleKey = (e: KeyboardEvent) => {
      // Close context menu / deselect
      if (e.key === "Escape") {
        setContextMenu(null);
        setSelectedApp(null);
      }

      // Enter — open selected app
      if (e.key === "Enter" && selectedAppRef.current) {
        const app = appsRef.current.find(a => a.app_id === selectedAppRef.current);
        if (app) handleAppDoubleClick(app);
      }

      // Ctrl+K or Alt+F2 — open voice command
      if ((e.ctrlKey || e.metaKey) && (e.key === "k" || e.key === "K")) {
        e.preventDefault();
        setShowVoice(true);
      }

      // Ctrl+F — focus search
      if ((e.ctrlKey || e.metaKey) && e.key === "f") {
        e.preventDefault();
        setIsSearchFocused(true);
        setTimeout(() => searchRef.current?.focus(), 50);
      }

      // Arrow keys — navigate grid
      if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.key)) {
        e.preventDefault();
        const filtered = filteredAppsRef.current;
        if (filtered.length === 0) return;
        const idx = filtered.findIndex(a => a.app_id === selectedAppRef.current);
        let next = 0;
        if (e.key === "ArrowRight") next = Math.min(idx + 1, filtered.length - 1);
        if (e.key === "ArrowLeft")  next = Math.max(idx - 1, 0);
        if (e.key === "ArrowDown")  next = Math.min(idx + GRID_COLS, filtered.length - 1);
        if (e.key === "ArrowUp")    next = Math.max(idx - GRID_COLS, 0);
        if (idx === -1) next = 0;
        setSelectedApp(filtered[next].app_id);
        // Scroll selected card into view
        document.getElementById(`app-card-${filtered[next].app_id}`)?.scrollIntoView({
          block: "nearest", behavior: "smooth"
        });
      }
    };
    window.addEventListener("click", handleClick);
    window.addEventListener("keydown", handleKey);
    return () => {
      window.removeEventListener("click", handleClick);
      window.removeEventListener("keydown", handleKey);
    };
  }, []);

  const isFirstLoad = useRef(true);

  const loadApps = async () => {
    if (isFirstLoad.current) setIsLoading(true);
    const r = await safeInvoke<AppItem[]>("get_installed_apps");
    if (r.ok) {
      setApps(r.data);
      if (isFirstLoad.current) {
        setIsLoading(false);
        isFirstLoad.current = false;
        const missingIcons = r.data.filter(a => !a.icon_base64).length;
        if (missingIcons > 0) pollForIcons();
      }
    } else {
      setIsLoading(false);
      isFirstLoad.current = false;
    }
  };

  const pollForIcons = () => {
    // No-op — replaced by real-time event listener below
  };

  const loadCategories = async () => {
    const r = await safeInvoke<string[]>("get_categories");
    if (r.ok) setCategories(r.data);
  };

  const handleAddCategory = async () => {
    if (newCategoryName.trim()) {
      await safeInvoke("add_category", { name: newCategoryName });  // fire and forget
      setNewCategoryName("");
      setShowAddCategory(false);
      await loadCategories();
    }
  };

  const handleMoveToCategory = async (app: AppItem, category: string) => {
    setContextMenu(null);
    setMovingApp({ name: app.name, to: category });
    const res = await safeInvoke("assign_app_to_category", { appId: app.app_id, category });
    if (res.ok) {
      await loadApps();
      showToast(`Moved "${app.name}" to ${category}`);
    }
    setMovingApp(null);
  };

  const handleAppClick = (app: AppItem) => {
    setSelectedApp(app.app_id === selectedApp ? null : app.app_id);
    setContextMenu(null);
  };

  const startSession = async (app: AppItem) => {
    activeSessionRef.current = { id: 0, app };
    await safeInvoke("log_app_open", {
      appId: app.app_id,
      appName: app.name,
      category: app.category,
    });
  };

  // Tell Rust to close the session when AppDrawer is hidden or closed
  // Only use visibilitychange — blur fires too aggressively (e.g. opening sub-panels)
  useEffect(() => {
    const handleVisibilityChange = async () => {
      if (document.visibilityState === "hidden") {
        await safeInvoke("log_app_close");
      }
    };
    const handleBeforeUnload = () => {
      safeInvoke("log_app_close");
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => {
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("beforeunload", handleBeforeUnload);
    };
  }, []);

  const handleAppDoubleClick = async (app: AppItem) => {
    const res = await safeInvoke("open_app", { appId: app.app_id });
    if (res.ok) {
      showToast(`Opened ${app.name}`);
      await startSession(app);
    }
    setSelectedApp(null);
  };

  const handleContextMenu = (e: React.MouseEvent, app: AppItem) => {
    e.preventDefault();
    e.stopPropagation();

    // Adjust position so menu stays within viewport
    const menuWidth = 200;
    const menuHeight = 160;
    const x = e.clientX + menuWidth > window.innerWidth ? e.clientX - menuWidth : e.clientX;
    const y = e.clientY + menuHeight > window.innerHeight ? e.clientY - menuHeight : e.clientY;

    setContextMenu({ x, y, app, showMoveTo: false });
  };

  // ── Drag & Drop ──
  const handleDragStart = (e: React.DragEvent, app: AppItem) => {
    setDraggingApp(app);
    draggingAppRef.current = app;
    e.dataTransfer.effectAllowed = "move";
    // Ghost image
    const ghost = document.createElement("div");
    ghost.textContent = app.name;
    ghost.style.cssText =
      "position:absolute;top:-9999px;background:#44576B;color:#fff;padding:6px 12px;border-radius:8px;font-size:13px;font-family:Segoe UI,sans-serif;";
    document.body.appendChild(ghost);
    e.dataTransfer.setDragImage(ghost, 0, 0);
    setTimeout(() => document.body.removeChild(ghost), 0);
  };

  const handleDragEnd = () => {
    setDraggingApp(null);
    draggingAppRef.current = null;
    setDragOverCategory(null);
  };

  const handleDragOverCategory = (e: React.DragEvent, category: string) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    setDragOverCategory(category);
  };

  const handleDropOnCategory = async (e: React.DragEvent, category: string) => {
    e.preventDefault();
    e.stopPropagation();
    const app = draggingAppRef.current;
    if (app && app.category !== category) {
      setMovingApp({ name: app.name, to: category });
      const res = await safeInvoke("assign_app_to_category", { appId: app.app_id, category });
      if (res.ok) {
        await loadApps();
        showToast(`Moved "${app.name}" to ${category}`);
      }
      setMovingApp(null);
    }
    setDraggingApp(null);
    draggingAppRef.current = null;
    setDragOverCategory(null);
  };

  // Keep refs in sync so keyboard handler always has current values
  selectedAppRef.current = selectedApp;
  appsRef.current = apps;

  const filteredApps = apps.filter((app) => {
    const matchesCategory = searchQuery ? true : app.category === selectedCategory;
    const matchesSearch = searchQuery
      ? app.name.toLowerCase().includes(searchQuery.toLowerCase())
      : true;
    return matchesCategory && matchesSearch;
  });
  filteredAppsRef.current = filteredApps;

  const getAppIcon = (app: AppItem): string | null => {
    if (app.icon_base64) return `data:image/png;base64,${app.icon_base64}`;
    return null;
  };

  const getInitials = (name: string) =>
    name.split(" ").map((w) => w[0]).join("").slice(0, 2).toUpperCase();

  const CATEGORY_ICONS: Record<string, string> = {
    Browser: "🌐",
    Entertainment: "🎬",
    Tools: "🔧",
    "System Applications": "⚙️",
    Uncategorized: "📦",
  };

  return (
    <div
      style={{
        display: "flex",
        height: "100vh",
        background: "#2E3A4A",
        color: "#E0E8F0",
        overflow: "hidden",
        fontFamily: "'Segoe UI Variable', 'Segoe UI', system-ui, sans-serif",
        userSelect: "none",
      }}
      onClick={(e) => { if (!(e.target as HTMLElement).closest("[data-ctx-menu]")) setContextMenu(null); }}
    >
      {/* ── Analytics Panel ── */}
      {showAnalytics && <Analytics onClose={() => setShowAnalytics(false)} />}

      {/* ── Voice Command ── */}
      {showVoice && (
        <VoiceCommand
          apps={apps}
          onClose={() => setShowVoice(false)}
          onOpenApp={(app: AppItem) => {
            setShowVoice(false);
            handleAppDoubleClick(app);
          }}
          onOpenCategory={(category: string) => {
            setSelectedCategory(category);
            setShowVoice(false);
          }}
          onOpenAnalytics={() => {
            setShowVoice(false);
            setShowAnalytics(true);
          }}
        />
      )}

      {/* ── Loading Screen ── */}
      {isLoading && (
        <div style={{
          position: "fixed",
          inset: 0,
          background: "#2E3A4A",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          zIndex: 9999,
          gap: 24,
        }}>
          <div style={{ fontSize: 48 }}>📦</div>
          <div style={{ fontSize: 18, fontWeight: 700, color: "#E0E8F0" }}>AppDrawer</div>
          <div style={{ color: "#8A9BAD", fontSize: 13 }}>Loading your apps...</div>
          <div style={{
            width: 200,
            height: 4,
            background: "rgba(255,255,255,0.08)",
            borderRadius: 4,
            overflow: "hidden",
          }}>
            <div style={{
              height: "100%",
              width: "40%",
              background: "#5FA8D3",
              borderRadius: 4,
              animation: "slide 1.2s ease-in-out infinite",
            }} />
          </div>
          <style>{`
            @keyframes slide {
              0% { transform: translateX(-100%); }
              100% { transform: translateX(600%); }
            }
          `}</style>
          <div style={{ color: "#8A9BAD", fontSize: 11, marginTop: 8 }}>
            First launch may take a moment while icons are extracted
          </div>
        </div>
      )}

      {/* ── Sidebar ── */}
      <div
        style={{
          width: 220,
          background: "#C8D0D8",
          display: "flex",
          flexDirection: "column",
          padding: "16px 12px",
          gap: 4,
          borderRight: "1px solid rgba(0,0,0,0.12)",
          overflowY: "auto",
        }}
      >
        <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
          {categories.map((category) => {
            const isActive = selectedCategory === category && !searchQuery;
            const isDragTarget = dragOverCategory === category;

            return (
              <button
                key={category}
                onClick={() => {
                  setSelectedCategory(category);
                  setSearchQuery("");
                  if (searchRef.current) searchRef.current.value = "";
                }}
                onDragOver={(e) => handleDragOverCategory(e, category)}
                onDragLeave={() => setDragOverCategory(null)}
                onDrop={(e) => handleDropOnCategory(e, category)}
                style={{
                  width: "100%",
                  padding: "12px 16px",
                  background: isDragTarget
                    ? "rgba(68,87,107,0.35)"
                    : isActive
                    ? "#44576B"
                    : "rgba(0,0,0,0.06)",
                  color: isActive ? "#fff" : "#2E3A4A",
                  border: isDragTarget
                    ? "2px dashed #44576B"
                    : isActive
                    ? "2px solid transparent"
                    : "2px solid transparent",
                  borderRadius: 10,
                  cursor: draggingApp ? "copy" : "pointer",
                  fontSize: 14,
                  fontWeight: isActive ? 600 : 500,
                  textAlign: "left",
                  transition: "all 0.15s ease",
                  display: "flex",
                  alignItems: "center",
                  gap: 10,
                  letterSpacing: 0.1,
                  transform: isDragTarget ? "scale(1.02)" : "scale(1)",
                }}
                onMouseEnter={(e) => {
                  if (!isActive && !isDragTarget)
                    e.currentTarget.style.background = "rgba(0,0,0,0.10)";
                }}
                onMouseLeave={(e) => {
                  if (!isActive && dragOverCategory !== category)
                    e.currentTarget.style.background = "rgba(0,0,0,0.06)";
                }}
              >
                <span style={{ fontSize: 16 }}>{CATEGORY_ICONS[category] ?? "📁"}</span>
                {category}
                {isDragTarget && (
                  <span style={{ marginLeft: "auto", fontSize: 11, opacity: 0.7 }}>Drop here</span>
                )}
              </button>
            );
          })}
        </div>

        {/* Analytics */}
        <button
          onClick={() => setShowAnalytics(true)}
          style={{
            width: "100%",
            padding: "12px 16px",
            background: "rgba(0,0,0,0.06)",
            color: "#2E3A4A",
            border: "none",
            borderRadius: 10,
            cursor: "pointer",
            fontSize: 14,
            fontWeight: 500,
            textAlign: "left",
            display: "flex",
            alignItems: "center",
            gap: 10,
            marginBottom: 4,
          }}
        >
          <span>📊</span> Analytics
        </button>

        {/* Voice Command */}
        <button
          onClick={() => setShowVoice(true)}
          style={{
            width: "100%",
            padding: "12px 16px",
            background: "rgba(0,0,0,0.06)",
            color: "#2E3A4A",
            border: "none",
            borderRadius: 10,
            cursor: "pointer",
            fontSize: 14,
            fontWeight: 500,
            textAlign: "left",
            display: "flex",
            alignItems: "center",
            gap: 10,
            marginBottom: 4,
          }}
        >
          <span>🎙</span> Voice Command
        </button>

        {/* Search */}
        <div style={{ marginTop: 8 }}>
          {!isSearchFocused && !searchQuery ? (
            <button
              onClick={() => {
                setIsSearchFocused(true);
                setTimeout(() => searchRef.current?.focus(), 50);
              }}
              style={{
                width: "100%",
                padding: "12px 16px",
                background: "rgba(0,0,0,0.06)",
                color: "#2E3A4A",
                border: "none",
                borderRadius: 10,
                cursor: "pointer",
                fontSize: 14,
                fontWeight: 500,
                textAlign: "left",
                display: "flex",
                alignItems: "center",
                gap: 10,
              }}
            >
              <span>🔍</span> Search Apps
            </button>
          ) : (
            <div style={{ position: "relative" }}>
              <span
                style={{
                  position: "absolute",
                  left: 12,
                  top: "50%",
                  transform: "translateY(-50%)",
                  fontSize: 14,
                  color: "#44576B",
                  pointerEvents: "none",
                }}
              >
                🔍
              </span>
              <input
                ref={searchRef}
                type="text"
                defaultValue={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onBlur={() => { if (!searchQuery) setIsSearchFocused(false); }}
                placeholder="Search apps..."
                style={{
                  width: "100%",
                  padding: "12px 12px 12px 36px",
                  background: "rgba(0,0,0,0.08)",
                  color: "#2E3A4A",
                  border: "1px solid #44576B",
                  borderRadius: 10,
                  fontSize: 14,
                  outline: "none",
                  boxSizing: "border-box",
                }}
              />
            </div>
          )}
        </div>

        {/* Add Category */}
        {!showAddCategory ? (
          <button
            onClick={() => setShowAddCategory(true)}
            style={{
              marginTop: 4,
              padding: "10px 16px",
              background: "rgba(0,0,0,0.06)",
              border: "none",
              borderRadius: 10,
              color: "#44576B",
              cursor: "pointer",
              fontWeight: 600,
              fontSize: 13,
              textAlign: "left",
              display: "flex",
              alignItems: "center",
              gap: 8,
            }}
          >
            <span>＋</span> Add Category
          </button>
        ) : (
          <div style={{ marginTop: 4, display: "flex", flexDirection: "column", gap: 6 }}>
            <input
              type="text"
              value={newCategoryName}
              onChange={(e) => setNewCategoryName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAddCategory()}
              placeholder="Category name..."
              autoFocus
              style={{
                width: "100%",
                padding: "10px 12px",
                borderRadius: 10,
                border: "1px solid #44576B",
                outline: "none",
                background: "rgba(0,0,0,0.08)",
                color: "#2E3A4A",
                fontSize: 13,
                boxSizing: "border-box",
              }}
            />
            <div style={{ display: "flex", gap: 6 }}>
              <button
                onClick={handleAddCategory}
                style={{ flex: 1, padding: 9, border: "none", borderRadius: 8, background: "#44576B", color: "#fff", cursor: "pointer", fontSize: 13, fontWeight: 600 }}
              >
                Add
              </button>
              <button
                onClick={() => { setShowAddCategory(false); setNewCategoryName(""); }}
                style={{ flex: 1, padding: 9, border: "none", borderRadius: 8, background: "rgba(0,0,0,0.10)", color: "#44576B", cursor: "pointer", fontSize: 13 }}
              >
                Cancel
              </button>
            </div>
          </div>
        )}
      </div>

      {/* ── Main Content ── */}
      <div style={{ flex: 1, padding: "32px 40px", overflowY: "auto", background: "#2E3A4A" }}>
        {/* Header */}
        <div style={{ marginBottom: 36 }}>
          <h1 style={{ margin: 0, fontSize: 28, fontWeight: 700, color: "#E0E8F0", letterSpacing: -0.3 }}>
            {searchQuery ? `Search: "${searchQuery}"` : selectedCategory}
          </h1>
          <p style={{ color: "#8A9BAD", marginTop: 6, fontSize: 13 }}>
            {filteredApps.length} {filteredApps.length === 1 ? "app" : "apps"}
            {draggingApp && (
              <span style={{ marginLeft: 12, color: "#7EC8E3" }}>
                ↖ Drag to a category in the sidebar
              </span>
            )}
          </p>
        </div>

        {/* App Grid */}
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(110px, 1fr))",
            gap: 20,
            maxWidth: 1200,
          }}
        >
          {filteredApps.map((app) => {
            const iconUrl = getAppIcon(app);
            const isSelected = selectedApp === app.app_id;
            const isDragging = draggingApp?.app_id === app.app_id;

            return (
              <div
                key={app.app_id}
                id={`app-card-${app.app_id}`}
                draggable
                onClick={(e) => { e.stopPropagation(); handleAppClick(app); }}
                onDoubleClick={() => handleAppDoubleClick(app)}
                onContextMenu={(e) => handleContextMenu(e, app)}
                onDragStart={(e) => handleDragStart(e, app)}
                onDragEnd={handleDragEnd}
                style={{
                  background: isSelected ? "rgba(126,200,227,0.18)" : "transparent",
                  border: isSelected ? "2px solid rgba(126,200,227,0.5)" : "2px solid transparent",
                  borderRadius: 16,
                  /* ┌─────────────────────────────────────────┐ */
                  /* │         APP CARD SIZE                   │ */
                  /* │  padding — inner spacing of each card    │ */
                  /* └─────────────────────────────────────────┘ */
                  padding: "10px 6px 8px",
                  cursor: "grab",
                  transition: "all 0.15s ease",
                  textAlign: "center",
                  display: "flex",
                  flexDirection: "column",
                  alignItems: "center",
                  gap: 10,
                  opacity: isDragging ? 0.4 : 1,
                  transform: isDragging ? "scale(0.95)" : "scale(1)",
                }}
                onMouseEnter={(e) => {
                  if (!isSelected) e.currentTarget.style.background = "rgba(255,255,255,0.06)";
                }}
                onMouseLeave={(e) => {
                  if (!isSelected) e.currentTarget.style.background = "transparent";
                }}
              >
                <div
                  style={{
                    width: 64,
                    height: 64,
                    borderRadius: 14,
                    overflow: "hidden",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    background: iconUrl ? "transparent" : "rgba(255,255,255,0.1)",
                    flexShrink: 0,
                    boxShadow: iconUrl ? "0 2px 10px rgba(0,0,0,0.3)" : "none",
                  }}
                >
                  {iconUrl ? (
                    <img
                      src={iconUrl}
                      alt={app.name}
                      style={{ width: 44, height: 44, /* ← APP ICON IMG SIZE (match icon box above) */ objectFit: "contain" }}
                      onError={(e) => { e.currentTarget.style.display = "none"; }}
                    />
                  ) : (
                    <span style={{ fontSize: 22, fontWeight: 700, color: "#8A9BAD", letterSpacing: -0.5 }}>
                      {getInitials(app.name)}
                    </span>
                  )}
                </div>
                <div
                  style={{
                    fontSize: 11,
                    color: isSelected ? "#E0E8F0" : "#C8D4DF",
                    wordBreak: "break-word",
                    lineHeight: 1.35,
                    maxWidth: "100%",
                    fontWeight: isSelected ? 600 : 400,
                  }}
                >
                  {app.name}
                </div>
              </div>
            );
          })}
        </div>

        {filteredApps.length === 0 && (
          <div style={{ marginTop: 100, textAlign: "center", color: "#8A9BAD" }}>
            <div style={{ fontSize: 48, marginBottom: 16 }}>{searchQuery ? "🔍" : "📂"}</div>
            <h2 style={{ margin: "0 0 8px", fontWeight: 600, color: "#C8D4DF" }}>
              {searchQuery ? "No results found" : "No apps in this category"}
            </h2>
            <p style={{ margin: 0, fontSize: 14 }}>
              {searchQuery
                ? `No apps match "${searchQuery}"`
                : "Right-click an app or drag it to a category in the sidebar."}
            </p>
          </div>
        )}
      </div>

      {/* ── Move Indicator ── */}
      {movingApp && (
        <div style={{
          position: "fixed",
          bottom: 32,
          right: 32,
          background: "#1E2B38",
          border: "1px solid rgba(255,255,255,0.1)",
          borderRadius: 14,
          padding: "14px 18px",
          display: "flex",
          alignItems: "center",
          gap: 14,
          boxShadow: "0 8px 32px rgba(0,0,0,0.4)",
          zIndex: 1999,
          minWidth: 260,
          animation: "slideInRight 0.2s ease",
        }}>
          {/* Spinning icon */}
          <div style={{
            width: 36,
            height: 36,
            borderRadius: 10,
            background: "rgba(95,168,211,0.15)",
            border: "2px solid #5FA8D3",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            fontSize: 16,
            flexShrink: 0,
            animation: "spin 1s linear infinite",
          }}>
            ↗
          </div>
          <div>
            <div style={{ fontSize: 13, fontWeight: 600, color: "#E0E8F0", marginBottom: 2 }}>
              Moving app...
            </div>
            <div style={{ fontSize: 11, color: "#8A9BAD", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", maxWidth: 180 }}>
              <span style={{ color: "#7EC8E3" }}>{movingApp.name}</span>
              {" → "}
              <span style={{ color: "#AAB8C5" }}>{movingApp.to}</span>
            </div>
          </div>
          <style>{`
            @keyframes slideInRight {
              from { opacity: 0; transform: translateX(20px); }
              to   { opacity: 1; transform: translateX(0); }
            }
            @keyframes spin {
              from { transform: rotate(0deg); }
              to   { transform: rotate(360deg); }
            }
          `}</style>
        </div>
      )}

      {/* ── Toast ── */}
      {toast && (
        <div style={{
          position: "fixed",
          bottom: 24,
          left: "50%",
          transform: "translateX(-50%)",
          background: toast.ok ? "#2D6A4F" : "#7B2D2D",
          color: "#fff",
          padding: "10px 20px",
          borderRadius: 10,
          fontSize: 13,
          fontWeight: 500,
          boxShadow: "0 4px 20px rgba(0,0,0,0.4)",
          zIndex: 2000,
          pointerEvents: "none",
          whiteSpace: "nowrap",
        }}>
          {toast.ok ? "✓" : "✕"} {toast.msg}
        </div>
      )}

      {/* ── Context Menu ── */}
      {contextMenu && (
        <div
          ref={contextMenuRef}
          data-ctx-menu
          onClick={(e) => e.stopPropagation()}
          style={{
            position: "fixed",
            top: contextMenu.y,
            left: contextMenu.x,
            zIndex: 1000,
            background: "#1E2B38",
            border: "1px solid rgba(255,255,255,0.1)",
            borderRadius: 10,
            padding: "6px",
            minWidth: 200,
            boxShadow: "0 8px 32px rgba(0,0,0,0.5), 0 2px 8px rgba(0,0,0,0.3)",
            backdropFilter: "blur(12px)",
          }}
        >
          {/* Open */}
          <ContextMenuItem
            label="Open"
            icon="↗"
            onClick={async () => {
              setContextMenu(null);
              const res = await safeInvoke("open_app", { appId: contextMenu.app.app_id });
              if (res.ok) {
                showToast(`Opened ${contextMenu.app.name}`);
                const sessionRes = await safeInvoke<number>("log_app_open", {
                  app_id: contextMenu.app.app_id,
                  app_name: contextMenu.app.name,
                  category: contextMenu.app.category,
                });
                if (sessionRes.ok && sessionRes.data) {
                  activeSessionRef.current = { id: sessionRes.data, app: contextMenu.app };
                        }
              }
            }}
          />

          <div style={{ height: 1, background: "rgba(255,255,255,0.08)", margin: "4px 0" }} />

          {/* Move To with submenu */}
          <div style={{ position: "relative" }}>
            <ContextMenuItem
              label="Move to"
              icon="→"
              hasSubmenu
              active={contextMenu.showMoveTo}
              onClick={() =>
                setContextMenu((prev) =>
                  prev ? { ...prev, showMoveTo: !prev.showMoveTo } : null
                )
              }
            />

            {contextMenu.showMoveTo && (
              <div
                style={{
                  position: "absolute",
                  left: "100%",
                  top: 0,
                  marginLeft: 4,
                  background: "#1E2B38",
                  border: "1px solid rgba(255,255,255,0.1)",
                  borderRadius: 10,
                  padding: "6px",
                  minWidth: 180,
                  boxShadow: "0 8px 32px rgba(0,0,0,0.5)",
                  zIndex: 1001,
                }}
              >
                {categories
                  .filter((c) => c !== contextMenu.app.category)
                  .map((category) => (
                    <ContextMenuItem
                      key={category}
                      label={category}
                      icon={CATEGORY_ICONS[category] ?? "📁"}
                      onClick={() => handleMoveToCategory(contextMenu.app, category)}
                    />
                  ))}
                {categories.filter((c) => c !== contextMenu.app.category).length === 0 && (
                  <div style={{ padding: "8px 12px", fontSize: 12, color: "#8A9BAD" }}>
                    No other categories
                  </div>
                )}
              </div>
            )}
          </div>

          <div style={{ height: 1, background: "rgba(255,255,255,0.08)", margin: "4px 0" }} />

          {/* Current category label */}
          <div style={{ padding: "6px 12px", fontSize: 11, color: "#8A9BAD" }}>
            Currently in: <strong style={{ color: "#AAB8C5" }}>{contextMenu.app.category}</strong>
          </div>
        </div>
      )}
    </div>
  );
}

// ── Small helper component ──
function ContextMenuItem({
  label,
  icon,
  hasSubmenu,
  active,
  onClick,
}: {
  label: string;
  icon: string;
  hasSubmenu?: boolean;
  active?: boolean;
  onClick: () => void;
}) {
  const [hovered, setHovered] = useState(false);

  return (
    <div
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onClick={onClick}
      style={{
        padding: "8px 12px",
        cursor: "pointer",
        fontSize: 13,
        color: "#E0E8F0",
        background: hovered || active ? "rgba(95,168,211,0.2)" : "transparent",
        display: "flex",
        alignItems: "center",
        gap: 10,
        borderRadius: 6,
        transition: "background 0.1s",
        userSelect: "none",
      }}
    >
      <span style={{ fontSize: 14, width: 18, textAlign: "center" }}>{icon}</span>
      <span style={{ flex: 1 }}>{label}</span>
      {hasSubmenu && <span style={{ fontSize: 10, opacity: 0.6 }}>▶</span>}
    </div>
  );
}