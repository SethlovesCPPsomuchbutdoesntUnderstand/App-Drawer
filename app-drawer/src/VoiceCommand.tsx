import { useState, useRef, useEffect } from "react";

// Browser Speech Recognition types
interface SpeechRecognitionEvent extends Event {
  results: SpeechRecognitionResultList;
}
interface SpeechRecognitionErrorEvent extends Event {
  error: string;
}
interface SpeechRecognitionInstance extends EventTarget {
  lang: string;
  interimResults: boolean;
  maxAlternatives: number;
  onstart: () => void;
  onresult: (e: SpeechRecognitionEvent) => void;
  onerror: (e: SpeechRecognitionErrorEvent) => void;
  onend: () => void;
  start: () => void;
  stop: () => void;
}
declare global {
  interface Window {
    SpeechRecognition: new () => SpeechRecognitionInstance;
    webkitSpeechRecognition: new () => SpeechRecognitionInstance;
  }
}
import { invoke } from "@tauri-apps/api/core";

interface AppItem {
  name: string;
  app_id: string;
  category: string;
}

interface AiAction {
  action: string;
  app_name: string | null;
  category: string | null;
  search_query: string | null;
  search_engine: string | null;
  response: string;
}

interface Props {
  apps: AppItem[];
  onClose: () => void;
  onOpenApp: (app: AppItem) => void;
  onOpenCategory: (category: string) => void;
  onOpenAnalytics: () => void;
}

const SEARCH_URLS: Record<string, string> = {
  google:  "https://www.google.com/search?q=",
  youtube: "https://www.youtube.com/results?search_query=",
  github:  "https://github.com/search?q=",
  default: "https://www.google.com/search?q=",
};

export default function VoiceCommand({ apps, onClose, onOpenApp, onOpenCategory, onOpenAnalytics }: Props) {
  const [isListening, setIsListening]   = useState(false);
  const [transcript, setTranscript]     = useState("");
  const [status, setStatus]             = useState<"idle" | "listening" | "thinking" | "done" | "error">("idle");
  const [response, setResponse]         = useState("");
  const [apiKey, setApiKey]             = useState("");
  const [showKeyInput, setShowKeyInput] = useState(false);
  const [history, setHistory]           = useState<{ cmd: string; res: string; ok: boolean }[]>([]);
  const recognitionRef = useRef<SpeechRecognitionInstance | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    // Load saved API key
    invoke<string>("load_api_key").then(k => {
      if (k) setApiKey(k);
      else setShowKeyInput(true);
    });
  }, []);

  const startListening = () => {
    const SpeechRecognition = window.SpeechRecognition || window.webkitSpeechRecognition;
    if (!SpeechRecognition) {
      setStatus("error");
      setResponse("Speech recognition not supported in this browser.");
      return;
    }

    const recognition = new SpeechRecognition();
    recognition.lang = "en-US";
    recognition.interimResults = true;
    recognition.maxAlternatives = 1;
    recognitionRef.current = recognition;

    recognition.onstart = () => {
      setIsListening(true);
      setStatus("listening");
      setTranscript("");
      setResponse("");
    };

    recognition.onresult = (e: SpeechRecognitionEvent) => {
      const t = Array.from(e.results)
        .map((r: SpeechRecognitionResult) => r[0].transcript)
        .join("");
      setTranscript(t);
      // Auto-submit on final result
      if (e.results[e.results.length - 1].isFinal) {
        recognition.stop();
        processCommand(t);
      }
    };

    recognition.onerror = (e: SpeechRecognitionErrorEvent) => {
      setIsListening(false);
      setStatus("error");
      setResponse(`Mic error: ${e.error}`);
    };

    recognition.onend = () => setIsListening(false);
    recognition.start();
  };

  const stopListening = () => {
    recognitionRef.current?.stop();
    setIsListening(false);
  };

  const processCommand = async (cmd: string) => {
    if (!cmd.trim()) return;
    setStatus("thinking");
    setTranscript(cmd);

    try {
      const action = await invoke<AiAction>("process_voice_command", {
        command: cmd,
        apiKey,
      });

      setResponse(action.response);
      setStatus("done");
      setHistory(prev => [{ cmd, res: action.response, ok: true }, ...prev.slice(0, 9)]);

      // Execute the action
      await executeAction(action);

    } catch (err) {
      const msg = typeof err === "string" ? err : JSON.stringify(err);
      setStatus("error");
      setResponse(`Error: ${msg}`);
      setHistory(prev => [{ cmd, res: msg, ok: false }, ...prev.slice(0, 9)]);
    }
  };

  const executeAction = async (action: AiAction) => {
    switch (action.action) {
      case "open_app": {
        if (!action.app_name) break;
        const match = apps.find(a =>
          a.name.toLowerCase().includes(action.app_name!.toLowerCase()) ||
          action.app_name!.toLowerCase().includes(a.name.toLowerCase())
        );
        if (match) {
          setTimeout(() => onOpenApp(match), 500);
        } else {
          setResponse(`Couldn't find "${action.app_name}" in your apps.`);
        }
        break;
      }
      case "search": {
        if (!action.search_query) break;
        const base = SEARCH_URLS[action.search_engine ?? "default"];
        const url = base + encodeURIComponent(action.search_query);
        await invoke("open_url", { url });
        break;
      }
      case "open_category": {
        if (!action.category) break;
        onOpenCategory(action.category);
        break;
      }
      case "analytics_query": {
        onOpenAnalytics();
        break;
      }
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && transcript.trim()) {
      processCommand(transcript);
    }
  };

  const saveKey = async () => {
    if (!apiKey.trim()) return;
    await invoke("save_api_key", { key: apiKey });
    setShowKeyInput(false);
  };

  const statusColor = {
    idle:      "#8A9BAD",
    listening: "#26C281",
    thinking:  "#F7B731",
    done:      "#5FA8D3",
    error:     "#FC5C65",
  }[status];

  const statusLabel = {
    idle:      "Say a command or type below",
    listening: "Listening...",
    thinking:  "Thinking...",
    done:      "Done",
    error:     "Error",
  }[status];

  return (
    <div style={{
      position: "fixed",
      inset: 0,
      background: "rgba(0,0,0,0.6)",
      backdropFilter: "blur(8px)",
      display: "flex",
      alignItems: "center",
      justifyContent: "center",
      zIndex: 6000,
      fontFamily: "'Segoe UI Variable', 'Segoe UI', system-ui, sans-serif",
    }}
    onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div style={{
        background: "#1A2530",
        border: "1px solid rgba(255,255,255,0.1)",
        borderRadius: 20,
        padding: 32,
        width: 540,
        maxHeight: "80vh",
        display: "flex",
        flexDirection: "column",
        gap: 20,
        boxShadow: "0 24px 80px rgba(0,0,0,0.6)",
      }}>
        {/* Header */}
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <div>
            <h2 style={{ margin: 0, fontSize: 20, fontWeight: 700, color: "#E0E8F0" }}>
              🎙 Voice Command
            </h2>
            <p style={{ margin: "4px 0 0", fontSize: 12, color: "#8A9BAD" }}>
              Powered by Claude AI
            </p>
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button
              onClick={() => setShowKeyInput(!showKeyInput)}
              style={{
                padding: "6px 12px", borderRadius: 8, border: "1px solid rgba(255,255,255,0.1)",
                background: "transparent", color: "#8A9BAD", cursor: "pointer", fontSize: 12,
              }}
            >
              🔑 API Key
            </button>
            <button
              onClick={onClose}
              style={{
                padding: "6px 12px", borderRadius: 8, border: "1px solid rgba(255,255,255,0.1)",
                background: "transparent", color: "#8A9BAD", cursor: "pointer", fontSize: 12,
              }}
            >
              ✕
            </button>
          </div>
        </div>

        {/* API Key Input */}
        {showKeyInput && (
          <div style={{
            background: "rgba(247,183,49,0.08)",
            border: "1px solid rgba(247,183,49,0.3)",
            borderRadius: 12, padding: 14, display: "flex", flexDirection: "column", gap: 8,
          }}>
            <p style={{ margin: 0, fontSize: 12, color: "#F7B731" }}>
              Enter your Anthropic API key — stored encrypted on your machine
            </p>
            <div style={{ display: "flex", gap: 8 }}>
              <input
                type="password"
                value={apiKey}
                onChange={e => setApiKey(e.target.value)}
                onKeyDown={e => e.key === "Enter" && saveKey()}
                placeholder="sk-ant-..."
                style={{
                  flex: 1, padding: "8px 12px", borderRadius: 8,
                  border: "1px solid rgba(255,255,255,0.15)",
                  background: "rgba(255,255,255,0.06)", color: "#E0E8F0",
                  fontSize: 13, outline: "none",
                }}
              />
              <button
                onClick={saveKey}
                style={{
                  padding: "8px 16px", borderRadius: 8, border: "none",
                  background: "#F7B731", color: "#1A2530", cursor: "pointer",
                  fontSize: 13, fontWeight: 600,
                }}
              >
                Save
              </button>
            </div>
          </div>
        )}

        {/* Mic Button */}
        <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 12 }}>
          <button
            onClick={isListening ? stopListening : startListening}
            disabled={status === "thinking"}
            style={{
              width: 80, height: 80, borderRadius: "50%",
              border: `3px solid ${statusColor}`,
              background: isListening
                ? "rgba(38,194,129,0.15)"
                : status === "thinking"
                ? "rgba(247,183,49,0.15)"
                : "rgba(255,255,255,0.05)",
              cursor: status === "thinking" ? "default" : "pointer",
              fontSize: 32,
              display: "flex", alignItems: "center", justifyContent: "center",
              transition: "all 0.2s ease",
              boxShadow: isListening ? `0 0 24px ${statusColor}66` : "none",
              animation: isListening ? "pulse 1.5s ease-in-out infinite" : "none",
            }}
          >
            {status === "thinking" ? "⏳" : isListening ? "⏹" : "🎙"}
          </button>
          <span style={{ fontSize: 13, color: statusColor, fontWeight: 500 }}>
            {statusLabel}
          </span>
        </div>

        {/* Transcript */}
        <div style={{
          background: "rgba(255,255,255,0.04)",
          border: "1px solid rgba(255,255,255,0.08)",
          borderRadius: 12, padding: 12,
        }}>
          <input
            ref={inputRef}
            type="text"
            value={transcript}
            onChange={e => setTranscript(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Or type your command here..."
            style={{
              width: "100%", background: "transparent", border: "none",
              outline: "none", color: "#E0E8F0", fontSize: 15,
              fontWeight: transcript ? 500 : 400,
              boxSizing: "border-box",
            }}
          />
        </div>

        {/* Response */}
        {response && (
          <div style={{
            background: status === "error"
              ? "rgba(252,92,101,0.1)"
              : "rgba(95,168,211,0.1)",
            border: `1px solid ${status === "error" ? "rgba(252,92,101,0.3)" : "rgba(95,168,211,0.3)"}`,
            borderRadius: 12, padding: 14,
          }}>
            <p style={{ margin: 0, fontSize: 14, color: status === "error" ? "#FC5C65" : "#7EC8E3", lineHeight: 1.5 }}>
              {status === "error" ? "✕ " : "✓ "}{response}
            </p>
          </div>
        )}

        {/* Quick commands hint */}
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
          {[
            "Open Brave",
            "Search YouTube for lo-fi",
            "Open my Tools",
            "Show analytics",
          ].map(hint => (
            <button
              key={hint}
              onClick={() => { setTranscript(hint); processCommand(hint); }}
              style={{
                padding: "5px 10px", borderRadius: 20,
                border: "1px solid rgba(255,255,255,0.1)",
                background: "rgba(255,255,255,0.04)",
                color: "#8A9BAD", cursor: "pointer", fontSize: 11,
                transition: "all 0.15s",
              }}
              onMouseEnter={e => {
                e.currentTarget.style.background = "rgba(95,168,211,0.15)";
                e.currentTarget.style.color = "#7EC8E3";
                e.currentTarget.style.borderColor = "rgba(95,168,211,0.4)";
              }}
              onMouseLeave={e => {
                e.currentTarget.style.background = "rgba(255,255,255,0.04)";
                e.currentTarget.style.color = "#8A9BAD";
                e.currentTarget.style.borderColor = "rgba(255,255,255,0.1)";
              }}
            >
              {hint}
            </button>
          ))}
        </div>

        {/* History */}
        {history.length > 0 && (
          <div style={{ maxHeight: 160, overflowY: "auto", display: "flex", flexDirection: "column", gap: 6 }}>
            <div style={{ fontSize: 11, color: "#8A9BAD", textTransform: "uppercase", letterSpacing: 0.8 }}>
              Recent
            </div>
            {history.map((h, i) => (
              <div
                key={i}
                onClick={() => { setTranscript(h.cmd); processCommand(h.cmd); }}
                style={{
                  padding: "8px 12px", borderRadius: 8, cursor: "pointer",
                  background: "rgba(255,255,255,0.03)",
                  border: "1px solid rgba(255,255,255,0.06)",
                  display: "flex", flexDirection: "column", gap: 2,
                }}
              >
                <span style={{ fontSize: 12, color: "#C8D4DF" }}>"{h.cmd}"</span>
                <span style={{ fontSize: 11, color: h.ok ? "#8A9BAD" : "#FC5C65" }}>{h.res}</span>
              </div>
            ))}
          </div>
        )}

        <style>{`
          @keyframes pulse {
            0%, 100% { transform: scale(1); }
            50% { transform: scale(1.08); }
          }
        `}</style>
      </div>
    </div>
  );
}