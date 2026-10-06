# 42 — Chart Tool Skills Use Case: AI Trading Assistant with Visual Chart Patterns

## Purpose

Enable the AI agent to **understand, communicate about, and execute chart analysis** using:
1. **Chart tools** (lines, shapes, measures, annotations)
2. **Pattern-specific analysis skills** (order blocks, swing points, absorption zones)
3. **Trading strategy skills** (methodology, risk rules, entry conditions)

This creates a **visual trading assistant** that sees your chart, understands your methodology, and executes your strategy while drawing patterns for you.

---

## How It Works Together

```
┌─────────────────────────────────────────────────────────────────────┐
│                         TRADER INPUT                                │
│  "Show me order blocks on this chart using my OB analysis skill"    │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    CAPABILITY VERIFICATION                          │
│  ✓ Chart drawing capability: AVAILABLE                            │
│  ✓ Order block detection: AVAILABLE                               │
│  ✓ Swing point detection: AVAILABLE                               │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    AGENT EXECUTION                                  │
│  1. Load OBA_Skill (Order Block Analysis v3)                      │
│  2. Analyze chart: find swing highs/lows                          │
│  3. Detect order blocks as rectangle patterns                     │
│  4. Use Chart Drawing Skill to draw rectangles                    │
│  5. Update annotations with OB zones                              │
│  6. Present findings with methodology explanation                 │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                       CHART RESULT                                  │
│  ✓ Orange rectangles: Bullish Order Blocks                        │
│  ✓ Purple rectangles: Bearish Order Blocks                        │
│  ✓ Blue lines: Swing Highs/Lows                                   │
│  ✓ "Detected 4 OB zones using OBA_Skill v3 methodology"         │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Skill File Structure for Chart Tools

### Example 1: Trend Line Skill (`skills/lines/trend-line.yaml`)

```yaml
name: "Trend Line Analysis"
version: "1.2"
category: "lines"
kind: trading
artifact_kind: thesis

capability_requirements:
  required:
    - chart_drawing  # MUST have line drawing capability
    - swing_detection  # MUST detect swing points
  preferred:
    - market_structure

knowledge: >
  Trend lines connects swing highs in downtrends and swing lows in uptrends.
  A valid trend line requires at least 2 touches. The more touches, the stronger
  the trend. Fade trades work best at well-tested trend lines.

rules:
  - "Draw trend line connecting swing lows for uptrend"
  - "Draw trend line connecting swing highs for downtrend"
  - "Wait for 2+ touches before confirming trend"
  - "Re-test of trend line is Entry Zone"
  - "Stop loss beyond trend line"
  - "Target 2x risk or prior swing"

conditions:
  timeframes: ["4h", "1h", "5m"]
  risk:
    max_risk_pct: 1.0

examples:
  - description: "BTCUSDT 4H: Uptrend trend line at 68,500, 3 touches, reclaimed +3.2R"
  - description: "ETHUSDT 1H: Downtrend trend line at 3,450, rejected, short +2.8R"

invalidation:
  - "Close beyond trend line with no re-test within 3 candles"

preferred_markets: ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
preferred_timeframes: ["4h", "1h", "15m"]
```

### Example 2: Order Block Analysis Skill (`skills/analysis/order-block.yaml`)

```yaml
name: "Order Block Analysis"
version: "3.0"
category: "analysis"
kind: trading
artifact_kind: thesis

capability_requirements:
  required:
    - chart_drawing  # Draw rectangles
    - market_structure  # Detect swings
    - absorption  # Confirm OB strength
  preferred:
    - delta
    - orderflow

knowledge: >
  Order Blocks are zones where institutional buying/selling occurred.
  Bullish OB = gap up zone with strong buying pressure.
  Bearish OB = gap down zone with strong selling pressure.
  OB re-tests are high-probability reversal zones.

rules:
  - "Identify OB as rectangle zone from swing low to high"
  - "Bullish OB: Located above current price, near support"
  - "Bearish OB: Located below current price, near resistance"
  - "Confirm OB with absorption on re-test"
  - "Entry on OB re-test with absorption confirmation"
  - "Stop loss beyond OB zone"
  - "Target 2-3x risk or next OB zone"

conditions:
  timeframes: ["4h", "1h", "5m"]
  risk:
    max_risk_pct: 1.0

examples:
  - description: "BTCUSDT 4H: Bullish OB at 68,200-68,500, re-tested with absorption, +4.1R"
  - description: "ETHUSDT 1H: Bearish OB at 3,420-3,450, rejected, +3.2R"

invalidation:
  - "Close beyond OB zone with no absorption"

preferred_markets: ["BTCUSDT", "ETHUSDT", "XRPUSDT"]
preferred_timeframes: ["4h", "1h", "15m"]
```

### Example 3: Chart Drawing Skill (`skills/tool/chart-drawing.yaml`)

```yaml
name: "Chart Drawing Tools"
version: "1.0"
category: "tool"
kind: tool
artifact_kind: thesis

applies_to:
  tools:
    - draw_line
    - draw_rectangle
    - draw_text
    - delete_annotation
    - update_annotation

knowledge: >
  Use chart annotations to mark your analysis areas.
  Lines for trends, support/resistance.
  Rectangles for zones like order blocks, absorption, market structure.
  Text labels for clear identification.

rules:
  - "Use draw_rectangle for zone patterns (OB, absorption, structure)"
  - "Use draw_line for trend lines, S/R levels"
  - "Use draw_text to label patterns clearly"
  - "Use delete_annotation to remove incorrect marks"
  - "Use update_annotation to modify existing marks"
  - "Never use annotations to make predictions"

conditions:
  timeframes: ["4h", "1h", "5m", "15m"]
  risk:
    max_risk_pct: 1.0

examples:
  - description: "Draw bullish OB rectangle at 68,200-68,500"
  - description: "Draw trend line at 3,420 for ETH downtrend"

invalidation:
  - "Markings must be accurate to chart data"

preferred_markets: ["all"]
preferred_timeframes: ["all"]
```

---

## How the Agent Uses These Skills

### Scenario 1: User Asks About Order Blocks

**User Input:**
```
"Analyze this chart for order blocks using my OB analysis skill"
```

**Agent Execution Flow:**

1. **Skill Retrieval:**
   ```
   - Loads "Order Block Analysis v3"
   - Checks: chart_drawing=Available ✓, market_structure=Available ✓
   ```

2. **Chart Analysis:**
   ```
   - Calls analyze_multi_timeframe for 4h/1h/5m
   - Extracts swing highs/lows
   - Identifies OB zones as price gaps + absorption
   ```

3. **Chart Drawing:**
   ```
   - Calls draw_rectangle for each OB zone
   - Bullish OB: Orange rectangle below price
   - Bearish OB: Purple rectangle above price
   - Calls draw_text with label: "Bullish OB v3"
   ```

4. **Thesis Generation:**
   ```
   - Uses skill methodology rules
   - Notes: 4 OB zones detected
   - Entry zones identified on re-test
   - Risk management parameters applied
   ```

### Scenario 2: User Asks About Trend Lines

**User Input:**
```
"Draw my trend line skill on this BTC chart"
```

**Agent Execution Flow:**

1. **Skill Retrieval:**
   ```
   - Loads "Trend Line Analysis v1.2"
   - Checks: chart_drawing=Available ✓, swing_detection=Available ✓
   ```

2. **Swing Detection:**
   ```
   - Identifies swing highs/lows across timeframes
   - Confirms trend structure
   ```

3. **Drawing:**
   ```
   - Draws uptrend line through swing lows (blue)
   - Draws downtrend line through swing highs (red)
   - Labels each line with trend strength score
   ```

### Scenario 3: User Asks About Multiple Patterns

**User Input:**
```
"Show me order blocks AND trend lines together"
```

**Agent Execution:**
```
- Combines OBA skill + Trend Line skill
- Draws rectangles for OB zones
- Draws lines for trends
- Creates analysis narrative
- "Found 4 OB zones + 2 active trend lines. Entry on re-tests."
```

---

## Data Contract Power

The **capability_requirements** field enables intelligent tool selection:

| Tool Available | Trend Line Skill | OB Analysis Skill |
|----------------|------------------|-------------------|
| chart_drawing ✓ | **Enabled** | **Enabled** |
| swing_detection ✓ | **Enabled** | **Enabled** |
| market_structure ✓ | Enables "Market Structure Break" strategy | Enables "OB+MS Bounce" strategy |
| absorption ✓ | Enables "Absorption Fade" strategy | **Core requirement** - OB validation |
| delta (derived) | Uses with caveat | Uses with caveat |

**Example:** If `absorption` is unavailable:
- OB Analysis skill shows warning: "Tick data unavailable - OB detection uses derived estimates"
- Agent still works but with reduced confidence labels

---

## Implementation Architecture

```
┌──────────────┐    ┌─────────────────────────────────┐
│   Frontend   │    │       Skills Storage            │
│  (UI Tabs)   │    │  - YAML files (shipped)         │
│              │    │  - Database (user-owned)        │
│  ┌─────────┐ │    │  - Category: lines/analysis/    │
│  │ Skills  │ │    │           tools/risk/market     │
│  └─────────┘ │    └─────────────────────────────────┘
│       ▲      │               │
│       │      │               ▼
│  ┌─────────┐ │    ┌─────────────────────────────────┐
│  │ Agent   │ │    │     Capabilities Registry       │
│  │  Chat   │ │    │  - Provider profiles            │
│  └─────▲───┘ │    │  - Symbol class support         │
│        │     │    │  - Tool availability mapping    │
│        └─────┘    └─────────────────────────────────┘
              │               │
              ▼               ▼
    ┌─────────────────────────────────────┐
    │      Agent Execution Engine         │
    │  - Retrieve skill by relevance      │
    │  - Verify capability requirements   │
    │  - Generate thesis with skill rules │
    │  - Call chart drawing tools         │
    └─────────────────────────────────────┘
              │
              ▼
    ┌─────────────────────────────────────┐
    │        Chart Visualization          │
    │  - Rectangles: OB, absorption      │
    │  - Lines: trends, S/R              │
    │  - Text: labels, explanations      │
    └─────────────────────────────────────┘
```

---

## Future Enhancements (Phase 5+)

1. **Skill Library Marketplace:**
   - Share skills publicly
   - Import skills from other traders
   - Skill ratings and reviews

2. **Automatic Chart Pattern Recognition:**
   - AI detects patterns first
   - User reviews and confirms
   - Skill applied to detected patterns

3. **Multi-Skill Chain Execution:**
   ```
   User: "Show me a full setup with my OBA skill + trend skill"
   
   Agent: 
   1. Load OBA skill
   2. Load Trend skill
   3. Find OB zones
   4. Find trend lines
   5. Mark confluence zones (OB + trend)
   6. Generate setup thesis
   ```

4. **Skill Versioning & A/B Testing:**
   - Compare skill v2 vs v3 performance
   - Track which methodology works best
   - Auto-select best skill for current market

---

## Documentation Mapping

| Use Case | Documentation |
|----------|---------------|
| Skill Basics | docs/10-SKILLS-SYSTEM.md |
| Skill v2 Schema | docs/40-SKILL-SCHEMA-V2.md |
| Tool Exposure | docs/41-PHASE-3-DYNAMIC-TOOL-EXPOSURE.md |
| Chart Tools | docs/14-FRONTEND-CHART-ENGINE.md |
| This Guide | docs/42-CHART-TOOL-SKILLS-USE-CASE.md |

---

## Key Takeaways

1. **Skills = Trading Methodology Codified**
   - Each skill is a reusable trading "recipe"
   - Works across any chart with required capabilities

2. **Chart Tools Enable Visual Execution**
   - Agent draws patterns on your chart
   - You see exactly what AI is analyzing

3. **Data Contracts Prevent Invalid Tools**
   - Skill fails gracefully if venue lacks data
   - Clear explanation of gaps

4. **Multiple Skills = Combined Analysis**
   - Chain skills for complex setups
   - Trend + OB + Absorption = Full setup

5. **Frontend UI Makes It Accessible**
   - Create skills via UI
   - Select skills in chat
   - See capabilities status
