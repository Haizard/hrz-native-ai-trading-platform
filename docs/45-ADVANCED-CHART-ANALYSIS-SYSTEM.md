# 45 — Advanced Multi-Timeframe Chart Analysis System

## Overview

This document outlines the architectural design for an advanced chart analysis system that enables the AI agent to:

1. **Open multiple chart panels** (multi-timeframe, multi-symbol)
2. **Take chart snapshots** for deeper analysis
3. **Use all chart tools** for pattern identification
4. **Cross-analyze** across timeframes and instruments
5. **Automate complex trading strategies** using chart-based insights

---

## 1. Multi-Panel Chart System

### 1.1 Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    CHART WORKSTATION                         │
│  ┌───────────────┐    ┌───────────────┐                     │
│  │   CHART 1     │    │   CHART 2     │                     │
│  │   (4H BTC)    │    │   (1H BTC)    │                     │
│  │  - Candle     │    │  - Candle     │                     │
│  │  - OB Zones   │    │  - S/R Levels │                     │
│  │  - Trend.Line │    │  - Fibonacci  │                     │
│  └───────────────┘    └───────────────┘                     │
│       ▲                       ▲                             │
│       └───────────┬───────────┘                             │
│                   │                    CHART SNAPSHOT        │
│                   ▼                      ENGINE              │
│              ┌─────────────────────┐                        │
│              │     DATA LAYER      │                        │
│              │  - candles         │                        │
│              │  - drawings        │                        │
│              │  - indicators      │                        │
│              └─────────────────────┘                        │
│                            │                                │
│                            ▼                                │
│                   ┌─────────────────┐                       │
│                   │   AI AGENT      │                       │
│                   │   - analyze()   │                       │
│                   │   - snapshot()  │                       │
│                   │   - compare()   │                       │
│                   └─────────────────┘                       │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 Implementation Plan

#### Backend Changes

**1. Chart Session Management**
```rust
// crates/ai-agent/src/chart_session.rs
pub struct ChartSession {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub charts: Vec<ChartPanel>,
    pub created_at: i64,
}

pub struct ChartPanel {
    pub id: String,
    pub chart_type: ChartType,  // Candle, Footprint, Heikin-Ashi
    pub symbol: String,
    pub timeframe: Timeframe,
    pub drawings: Vec<UserDrawing>,
    pub indicators: Vec<Indicator>,
    pub snapshot_id: Option<String>,
}
```

**2. New API Endpoints**
```
POST   /chart-sessions           - Create new chart session
GET    /chart-sessions           - List chart sessions
GET    /chart-sessions/{id}      - Get chart session
PUT    /chart-sessions/{id}      - Update chart session
DELETE /chart-sessions/{id}      - Delete chart session

POST   /chart-sessions/{id}/charts        - Add chart panel
DELETE /chart-sessions/{id}/charts/{cid}  - Remove chart panel

POST   /chart-sessions/{id}/snapshots    - Take snapshot
GET    /chart-sessions/{id}/snapshots    - List snapshots
GET    /chart-sessions/{id}/snapshots/{sid}  - Get snapshot
```

**3. Chart Panel Tools**
```rust
// New tools for multi-panel management
- open_chart_panel(symbol, timeframe, chart_type)
- close_chart_panel(panel_id)
- move_chart_panel(panel_id, to_row, to_column)
- resize_chart_panel(panel_id, width_pct)
- duplicate_chart_panel(panel_id)
- sync_panels(to_panel_id)  // Sync timeframe/symbol to another panel
```

#### Frontend Changes

**1. UI Components**
```javascript
// app.js additions
function createChartRow() { /* Add row with grid layout */ }
function createChartPanel(row, index) { /* Add panel to row */ }
function toggleChartPanelExpand(panel) { /* Maximize/minimize */ }

// Right-click menu additions
- "Open New Panel" → open new chart panel
- "Duplicate Panel" → duplicate current panel
- "Move Panel" → move to different row/column
- "Close Panel" → close panel
- "Sync Panels" → sync with another panel
```

---

## 2. Chart Snapshot System

### 2.1 Architecture

```
┌────────────────────────────────────────────────────┐
│              CHART SNAPSHOT SERVICE                │
│                                                    │
│  ┌──────────────────┐    ┌─────────────────────┐  │
│  │  Take Snapshot   │    │   Store Snapshot    │  │
│  │  - Panel state   │    │   - Panel state     │  │
│  │  - Candle data   │    │   - Drawings        │  │
│  │  - Drawings      │    │   - Indicators      │  │
│  │  - Indicator     │    │   - Timestamp       │  │
│  └──────────────────┘    └─────────────────────┘  │
│           │                          │             │
│           ▼                          ▼             │
│    ┌─────────────┐           ┌───────────────┐    │
│    │  In-Memory  │           │  Database/    │    │
│    │  Cache      │           │  File System  │    │
│    └─────────────┘           └───────────────┘    │
│           │                          │             │
│           └──────────┬───────────────┘             │
│                      ▼                             │
│            ┌─────────────────┐                     │
│            │  Snapshot ID    │                     │
│            │  (UUID v4)      │                     │
│            └─────────────────┘                     │
└────────────────────────────────────────────────────┘
```

### 2.2 Implementation Plan

**1. Snapshot Structure**
```rust
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub session_id: String,
    pub panel_id: String,
    pub timestamp: i64,
    pub name: Option<String>,
    pub tags: Vec<String>,
    pub panel_state: PanelState,
    pub scene_data: SceneData,  // Engine scene output
}

#[derive(Serialize, Deserialize)]
pub struct PanelState {
    pub symbol: String,
    pub timeframe: String,
    pub chart_type: String,
    pub viewport: Viewport,
    pub drawings: Vec<UserDrawing>,
    pub indicators: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SceneData {
    pub candles: Vec<Candle>,
    pub drawings: Vec<Drawing>,
    pub indicators: Vec<IndicatorPlot>,
    pub regions: Vec<Region>,
    pub zones: Vec<Zone>,
}
```

**2. Snapshot Tools for AI Agent**
```rust
// New tools
- take_snapshot(symbol, timeframe, name, tags)
  → Returns snapshot_id

- get_snapshot(snapshot_id)
  → Returns full snapshot data

- list_snapshots(symbol, timeframe, tags)
  → Returns list of snapshots

- compare_snapshots(snapshot_id_1, snapshot_id_2)
  → Returns differences between snapshots

- apply_snapshot(snapshot_id)
  → Reconstruct panel to snapshot state
```

**3. Agent Integration**
```rust
// Agent can now:
1. Take snapshot before making changes
2. Save snapshot after analyzing pattern
3. Compare multiple snapshots for pattern recognition
4. Apply previous snapshot to compare with current state
```

---

## 3. Comprehensive Chart Tool System

### 3.1 Existing Tools (Already Implemented)

| Tool | Category | Description |
|------|----------|-------------|
| `draw_rectangle` | Drawing | Draw OB zones, absorption zones |
| `draw_line` | Drawing | Draw trendlines, S/R levels |
| `draw_text` | Drawing | Add labels and annotations |
| `update_annotation` | Drawing | Modify existing annotations |
| `delete_annotation` | Drawing | Remove annotations |
| `get_user_drawings` | Reading | Read user's existing drawings |

### 3.2 New Chart Analysis Tools

**1. Measure Tools**
```rust
- measure_distance(x1, y1, x2, y2) → distance in price/time
- measure_angle(line_id) → angle in degrees
- measure_fibonacci(from_time, from_price, to_time, to_price) → fib levels
```

**2. Pattern Detection Tools**
```rust
- detect_pattern(pattern_type, symbol, timeframe)
  → Supports: head_and_shoulders, double_top, double_bottom,
              triangle, wedge, flag, pennant, channel

- find_support_resistance(symbol, timeframe, count) → list of S/R levels
- find_trend_lines(symbol, timeframe, min_touches) → list of trend lines
- find_order_blocks(symbol, timeframe) → list of OB zones
- find_liquidity_levels(symbol, timeframe) → list of liquidity zones
```

**3. Technical Indicator Tools**
```rust
- calculate_rsi(symbol, timeframe, period) → RSI values
- calculate_macd(symbol, timeframe, fast, slow, signal) → MACD values
- calculate_sma(symbol, timeframe, period) → SMA values
- calculate_ema(symbol, timeframe, period) → EMA values
- calculate_bollinger_bands(symbol, timeframe, period, stddev) → BB values
- calculate_atr(symbol, timeframe, period) → ATR values
- calculate_volume_profile(symbol, timeframe, bins) → volume profile
```

**4. Multi-Timeframe Analysis Tools**
```rust
- analyze_timeframe_hierarchy(symbol, timeframes[]) → analysis for each
- compare_timeframes(symbol, tf1, tf2) → relationship analysis
- cross_timeframe_confluence(symbol, timeframes) → confluence zones
- higher_timeframe_bias(symbol, tf_upper, tf_lower) → HTF trend bias
```

**5. Chart Geometry Tools**
```rust
- get_price_at_time(symbol, timeframe, time_ms) → price at timestamp
- get_time_at_price(symbol, timeframe, price) → time at price level
- get_bar_range(symbol, timeframe, from, to) → OHLC for range
- get_pivot_points(symbol, timeframe) → pivot levels
```

### 3.3 Tool Schema Example

```rust
ToolSpec {
    name: "detect_pattern".into(),
    description: "Detect chart patterns: head_and_shoulders, double_top, double_bottom, triangle, wedge, flag, pennant, channel. Returns pattern boundaries, confidence, and projected target.".into(),
    input_schema: json!({
        "type": "object",
        "properties": {
            "pattern_type": {"type": "string", "enum": ["head_and_shoulders", "double_top", "double_bottom", "triangle", "wedge", "flag", "pennant", "channel"]},
            "symbol": {"type": "string"},
            "timeframe": {"type": "string", "enum": ["1m", "5m", "15m", "1h", "4h", "1d"]},
            "min_confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.7}
        },
        "required": ["pattern_type", "symbol", "timeframe"]
    }),
    exposed_tool: Some("chart_analysis"),
}
```

---

## 4. Advanced AI Agent Capabilities

### 4.1 Multi-Panel Workflow Example

```
User Request:
"Analyze BTCUSDT using multi-timeframe approach with 4H, 1H, and 5M panels"

Agent Actions:
1. opens_chart_panel("BTCUSDT", "4h", "candle")
2. opens_chart_panel("BTCUSDT", "1h", "candle")
3. opens_chart_panel("BTCUSDT", "5m", "candle")
4. identifies_htf_structure("BTCUSDT", ["4h", "1h", "5m"])

Result:
4H Panel: Identifies bullish structure, HH/HL pattern
1H Panel: Finds OB zone at 68,200-68,500
5M Panel: Measures re-test at OB with absorption
```

### 4.2 Snapshot-Based Analysis

```
User Request:
"Take snapshot of current analysis, then analyze if OB zone holds"

Agent Actions:
1. take_snapshot("BTCUSDT 4H", tags=["ob_analysis", "entry_zone"])
2. wait_for_price_action()
3. take_snapshot("BTCUSDT 4H", tags=["ob_retest", "result"])

4. compare_snapshots(snapshot_before, snapshot_after)
   → Confirmed OB hold: Price rejected from 68,500
   → Re-test count: 1 of 3 expected
   → Absorption confirmed: Delta divergence present
```

### 4.3 Pattern Recognition Workflow

```
User Request:
"Find all major chart patterns on ETHUSDT 4H"

Agent Actions:
1. open_chart_panel("ETHUSDT", "4h", "candle")
2. detect_pattern("head_and_shoulders", "ETHUSDT", "4h")
3. detect_pattern("double_top", "ETHUSDT", "4h")
4. detect_pattern("double_bottom", "ETHUSDT", "4h")
5. detect_pattern("triangle", "ETHUSDT", "4h")

Result:
- Head and Shoulders at 3,500 (bearish, confidence 0.85)
- Triangle pattern at 3,400-3,450 (bullish breakout expected)
- No double tops or bottoms detected
```

---

## 5. Implementation Dependencies

### 5.1 Backend Dependencies

```
crates/ai-agent/src/
├── chart_session.rs      (NEW - session management)
├── snapshot.rs           (NEW - snapshot service)
├── chart_tools.rs        (NEW - comprehensive chart tools)
└── agent.rs              (MODIFIED - multi-panel workflow)
```

### 5.2 Frontend Dependencies

```
frontend/app/
├── app.js                (MODIFIED - multi-panel UI)
├── index.html            (MODIFIED - panel layout CSS)
└── (new) chart_panels.js (NEW - panel management)
```

### 5.3 Database Schema

```sql
-- New tables for chart sessions
CREATE TABLE chart_sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    name VARCHAR(255),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE chart_panels (
    id UUID PRIMARY KEY,
    session_id UUID REFERENCES chart_sessions(id),
    symbol VARCHAR(32) NOT NULL,
    timeframe VARCHAR(8) NOT NULL,
    chart_type VARCHAR(16) DEFAULT 'candle',
    row_index INT DEFAULT 0,
    col_index INT DEFAULT 0,
    is_expanded BOOLEAN DEFAULT FALSE,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE chart_snapshots (
    id UUID PRIMARY KEY,
    panel_id UUID REFERENCES chart_panels(id),
    name VARCHAR(255),
    tags JSONB,
    panel_state JSONB,
    scene_data JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE chart_snippets (
    id UUID PRIMARY KEY,
    panel_id UUID REFERENCES chart_panels(id),
    snapshot_id UUID REFERENCES chart_snapshots(id),
    label VARCHAR(255),
    data JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW()
);
```

---

## 6. Advanced Features

### 6.1 Chart Templates
Save/restore chart configurations (symbol, timeframe, indicators, drawings)

### 6.2 Pattern Library
Save/find common patterns across multiple charts

### 6.3 Multi-Chart Comparison
Side-by-side comparison of same symbol on different timeframes

### 6.4 Automated Pattern Scanning
Scan symbols for patterns, alert on matches

### 6.5 Chart Analysis Notes
Attach analysis notes to specific chart regions

---

## 7. Migration Path

### Phase 1: Multi-Panel Infrastructure (Week 1-2)
- [ ] Chart session management backend
- [ ] Panel CRUD endpoints
- [ ] Frontend panel layout
- [ ] Panel controls (close, expand)

### Phase 2: Snapshot System (Week 3-4)
- [ ] Snapshot service implementation
- [ ] Snapshot storage
- [ ] Agent snapshot tools
- [ ] Frontend snapshot UI

### Phase 3: Comprehensive Chart Tools (Week 5-6)
- [ ] Pattern detection tools
- [ ] Measure tools
- [ ] Technical indicator tools
- [ ] Multi-timeframe analysis tools

### Phase 4: Agent Integration (Week 7-8)
- [ ] Multi-panel workflow
- [ ] Snapshot-based analysis
- [ ] Pattern recognition
- [ ] Cross-timeframe confluence

### Phase 5: Advanced Features (Week 9-10)
- [ ] Chart templates
- [ ] Pattern library
- [ ] Automated scanning
- [ ] Chart analysis notes

---

## 8. Key Benefits

1. **Visual Multi-Timeframe Analysis** - See trends across timeframes simultaneously
2. **Pattern Recognition Automation** - AI finds patterns faster than manual scanning
3. **Analysis Reproducibility** - Snapshots allow exact reproduction of analysis
4. **Context Preservation** - Multi-panel preserves context across timeframes
5. **Complex Strategy Implementation** - Chart-based logic for sophisticated strategies

---

## 9. Related Documentation

- `docs/09-AI-AGENT-SYSTEM.md` - AI Agent System
- `docs/14-FRONTEND-CHART-ENGINE.md` - Frontend & Chart Engine
- `docs/19-DRAWINGS-GATEWAY.md` - Drawings Gateway
- `docs/40-SKILL-SCHEMA-V2.md` - Skills Schema v2
- `docs/42-CHART-TOOL-SKILLS-USE-CASE.md` - Chart Tools Skills Use Case

---

## 10. Next Steps

1. Implement chart session management backend
2. Add multi-panel UI to frontend
3. Create snapshot system
4. Implement chart analysis tools
5. Integrate agent with new capabilities
6. Test with real trading scenarios
