/**
 * Torrent-style session Info panel for the viewer.
 *
 * Renders in the app content area (not a modal). It does not sample: `Session`
 * owns the app's one sampler and this pane repaints on the `tick` it publishes.
 * Two samplers would each compute the other's second reading over a few
 * milliseconds with no byte delta, overwriting the real rates with zero — the
 * counters would climb while the UI insisted nothing was moving.
 *
 * That tick is 1s, and the interval *is* the averaging window for the up/down
 * rates, since they come from differencing cumulative counters.
 *
 * ## Layout
 *
 * A bento of panels rather than one column of labelled lines, because the
 * questions this pane answers are separate ones — how fast is it going, who is
 * it talking to, what is carrying the bytes — and a flat list gives every fact
 * the same weight. One column on a narrow window, two above `WIDE_PX`, with the
 * peer table spanning both because its columns have fixed widths.
 *
 * Panels are separated by a raised background rather than by a rule — see
 * `Panel`. Six bordered boxes on one screen is six rectangles competing with
 * the peer table's own rules; a shade change groups just as well and leaves the
 * ink for the data.
 */

import {
  Badge,
  Button,
  ProgressBar,
  Stack,
  StatusDot,
  Table,
  Text,
  rows,
  t,
} from "moonspace-dom";
import { component, disposable, interval, listen, signal } from "visage-dom";
import type { ReadonlySignal } from "visage-dom";
import { Style, css } from "visage-style";

import {
  missingSlots,
  peerAvailability,
  slotCoverage,
} from "./availability/index.ts";
import {
  CELLS,
  GRID_SIDE,
  PEER_CELLS,
  isCoarse,
  resampleSlots,
} from "./availabilityGrid/index.ts";
import { sortPeers } from "./peers/index.ts";
import type { PeerAvailability } from "./availability/index.ts";
import {
  formatIpWithFlag,
  isGeoLookupCandidate,
  lookupCountryCode,
} from "./countryFlag/index.ts";
import { shareProgress } from "./progress/index.ts";
import { SlotGrid } from "./SlotGrid/index.tsx";
import { sparkline } from "./sparkline/index.ts";
import type { RateSample } from "../Session/session.ts";
import { Bento, Panel } from "../Panel/index.tsx";
import {
  agentActivity,
  subscribeAgentActivity,
  type AgentActivity,
} from "agent-share-core/agentTools";
import { fileSeedState, type Coverage } from "agent-share-core/seeding";
import { formatRate, laneSummary } from "agent-share-core/transferStats";
import type { LinkSample } from "agent-share-core/transferStats";
import { humanBytes } from "agent-share-core/tree";
import type { FileNode } from "agent-share-core/tree";

export interface InfoClient {
  info(): unknown;
  refresh_peer_ips(): Promise<void>;
}

export interface TechInfoProps {
  client: InfoClient;
  /** The session sampler's tick. Read during render, to repaint on each one. */
  tick: ReadonlySignal<number>;
  /** Every file in the manifest, in slot order. The share's whole address space. */
  files: readonly FileNode[];
  /** Slots this tab holds in full, and how much of the partial ones. */
  held: ReadonlySignal<ReadonlySet<number>>;
  coverage: ReadonlySignal<Coverage>;
  /** The last minute of rates, for the sparklines. */
  history: ReadonlySignal<readonly RateSample[]>;
  openedAt: number;
  lastActivityAt: ReadonlySignal<number>;
  /** ready / mounting / syncing / downloading / mounted */
  status: string;
  mounted: boolean;
  mountError: string | null;
  /** Reveal the dev tools. Set by `?dev=true`; off for anyone handed a link. */
  dev: boolean;
  /** Close the mount connection, so the reconnect path can be exercised. */
  onKillConnection: () => void;
  /** True while a reconnect is already running — nothing left to kill. */
  killDisabled: boolean;
  onClose: () => void;
}

interface PeerRow {
  id: string;
  role: string;
  flags: string;
  client: string;
  version: string | null;
  ip: string | null;
  ip_kind: string | null;
  proto: string;
  /** Wire bytes on the selected ICE pair — includes SCTP/DTLS/STUN framing. */
  bytes_sent: number;
  bytes_received: number;
  /** Bytes/second since the previous sample; 0 until there are two. */
  up_bps: number;
  down_bps: number;
  /** Round-trip time in ms, or null when the pair has not been measured. */
  rtt_ms: number | null;
  /**
   * Which manifest slots this peer says it can serve — `*`, run-length ranges,
   * or absent when it has not said. See `availability.ts`.
   */
  serving: string | null;
  /**
   * Manifest fingerprint those slot numbers index into. Squares from peers on
   * different trees do not line up and must not be drawn as though they do.
   */
  tree: string | null;
}

interface RelayRow {
  url: string;
  /** iroh homed on this one, so it has a live connection state to report. */
  home: boolean;
  connected: boolean;
  last_error: string | null;
}

interface SessionInfo {
  general: {
    transport: string;
    identity_fingerprint: string;
    mesh_up: boolean;
    nickname: string | null;
    local_endpoint: string;
    producer_endpoint: string;
    connected_ms_ui: number;
  };
  trackers: {
    relays: RelayRow[];
    producer_reach: { mdns: boolean; dht: boolean; relay: string };
  };
  swarm: {
    peers_gossip: number;
    peers_direct: number;
    max_direct: number;
    peers: PeerRow[];
  };
  transfer: {
    mount_mode: string;
    mount_path: string;
    mount_paths: string[];
    /** Why `dynamic` ended up on the relay. Null on a clean WebRTC connect. */
    mount_fallback_reason: string | null;
    /** The manifest version this tab is on; 0 before the first watch frame. */
    manifest_version: number;
    /** Whether the tree is coming from the creator or from a peer relaying it. */
    source: string;
    /**
     * Wire bytes on the mount connection, per path and in total — the last
     * reading the session sampler took, never a fresh one. See `link.rs`.
     */
    link: LinkSample;
  };
}

/**
 * The peer table's wrapper: a horizontal escape, since its columns have fixed
 * widths, and one override.
 *
 * `Table` draws its header rule in `border`, which is right on a page of
 * bordered components and wrong on this one — every other frame is gone, so the
 * last remaining rule reads as a stray. Matching the column headers it sits
 * under makes it part of the header rather than a leftover of the box.
 */
const PEER_TABLE = css({
  overflowX: "auto",
  /*
    The attribute is doubled to win, and it has to be. `Table` compiles into the
    same `moonspace` layer this does and at the same specificity, so the tie
    breaks on source order — and its `<style>` sits *inside* the table, which is
    after this one. Repeating the selector makes it (0,2,0) against (0,1,0) and
    takes order out of it.
  */
  "[data-ms-rule][data-ms-rule]::before": { borderTopColor: t.fgMuted },
});

/**
 * How tall the WebMCP log is, in rows.
 *
 * A fixed height rather than a `min`/`max` pair, because the panel sits beside
 * another one: a box that grew with the number of calls would move its
 * neighbour's row every time an agent did anything. Twelve is about what the
 * panels next to it come to.
 */
const LOG_ROWS = 12;

/**
 * One peer's availability as a line of squares, the way a BitTorrent client
 * paints pieces.
 *
 * Always `PEER_CELLS` wide, whatever the share's size — the column has a fixed
 * width and a line that grew with the manifest would either overflow it or
 * shrink to invisibility. `resampleSlots` folds the share onto that line.
 *
 * Three states rather than two, and the third is the point: squares for a peer
 * that has answered, and a phrase for one that has not. Drawing an all-empty
 * line for the second case would claim the peer holds nothing, when what we
 * actually know is that it has not said.
 */
function PeerSlots(props: {
  peer: PeerAvailability;
  total: number;
  ourTree: string | null;
}) {
  if (props.peer.unknown) {
    return <Text color="fgMuted">not published</Text>;
  }
  // A slot index is meaningless across trees, so say so rather than paint
  // squares that appear to line up with everyone else's.
  if (props.ourTree && props.peer.tree && props.peer.tree !== props.ourTree) {
    return <Text color="fgMuted">≠ tree</Text>;
  }
  const held = new Set(props.peer.held);
  const filled = props.peer.complete ? props.total : held.size;
  return (
    // A flex box the full height of the cell, so the line sits on the row's
    // centre rather than on its baseline, where it reads as having slipped.
    <span
      title={`${filled} of ${props.total} slots`}
      style={{ display: "flex", alignItems: "center", height: "100%" }}
    >
      <SlotGrid
        states={resampleSlots(props.total, PEER_CELLS, (slot) =>
          props.peer.complete || held.has(slot) ? "full" : "none",
        )}
      />
    </span>
  );
}

function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

/** Ping to one decimal — sub-millisecond on a loopback pair is normal. */
function pingLabel(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  return `${ms < 10 ? ms.toFixed(1) : Math.round(ms)} ms`;
}

function callsLabel(calls: number): string {
  return calls === 1 ? "1 call" : `${calls} calls`;
}

function dash(value: string | null | undefined): string {
  return value && value.length > 0 ? value : "—";
}

function shortId(id: string): string {
  if (id.length <= 20) return id;
  return `${id.slice(0, 8)}…${id.slice(-8)}`;
}

/** Wall clock as `14:32:07`, since anything older than a session is impossible. */
function clockLabel(at: number): string {
  if (at <= 0) return "—";
  return new Date(at).toLocaleTimeString();
}

/**
 * Sent over received.
 *
 * Wire bytes, and labelled as such wherever it is shown: on a tab that has only
 * consumed, `sent` is mostly acknowledgements, so a ratio of 0.02 is what a
 * healthy download looks like rather than a reproach.
 */
function ratioLabel(link: LinkSample | null): string {
  if (!link || link.total.received <= 0) return "—";
  return (link.total.sent / link.total.received).toFixed(2);
}

function capabilitiesLine(): string {
  const parts: string[] = [];
  // Not a can/cannot any more: every browser can share through a file input.
  // What the picker decides is whether a share follows the folder or is pinned
  // to what was picked, and whether a mount can write back to disk.
  parts.push(
    typeof window.showDirectoryPicker === "function" ? "FSA" : "snapshot-only",
  );
  parts.push(typeof RTCPeerConnection === "function" ? "WebRTC" : "no WebRTC");
  parts.push(window.isSecureContext ? "secure" : "insecure");
  return parts.join(" · ");
}

function readInfo(client: InfoClient): SessionInfo | null {
  try {
    return client.info() as SessionInfo;
  } catch {
    return null;
  }
}

/**
 * The sparkline's width in cells. One cell is one sampler tick.
 *
 * Forty rather than the full minute the history holds, so the graph, its glyph
 * and its peak label still fit one line inside a single-column panel — where
 * the label was wrapping and pushing the two graphs apart.
 */
const SPARK_CELLS = 40;

/**
 * One labelled rate history: the glyph, the line, and the peak it is drawn
 * against.
 *
 * The peak is not decoration — the line is scaled to its own window, so without
 * it a full-height graph could be a megabyte a second or a trickle.
 */
function Spark(props: {
  glyph: string;
  label: string;
  values: readonly number[];
}) {
  const peak = Math.max(0, ...props.values);
  return (
    <Stack direction="row" gap={1}>
      <Text color="fgMuted" aria-hidden="true">
        {props.glyph}
      </Text>
      <Text
        color="fgMuted"
        aria-label={`${props.label} over the last ${props.values.length} seconds, peaking at ${formatRate(peak)}`}
        style={{ whiteSpace: "pre" }}
      >
        {sparkline(props.values, SPARK_CELLS)}
      </Text>
      {/*
        `nowrap`, because this label wrapping is what pushes the two graphs
        apart — and two rows of `█` that no longer share a baseline read as a
        rendering fault rather than as a narrow window.
      */}
      <Text color="fgMuted" style={{ whiteSpace: "nowrap" }}>
        peak {formatRate(peak)}
      </Text>
    </Stack>
  );
}

export const TechInfo = component<TechInfoProps>(function* (props) {
  // The nested plain function below captures `ctx`; `this` would not reach it.
  const ctx = this;
  /** IP → ISO country code (or `null` after a failed / non-candidate lookup). */
  const countries = signal<Record<string, string | null>>({});
  const inflight = new Set<string>();

  function requestCountry(ip: string): void {
    if (!isGeoLookupCandidate(ip)) return;
    if (Object.hasOwn(countries.peek(), ip) || inflight.has(ip)) return;
    inflight.add(ip);
    void lookupCountryCode(ip).then((code) => {
      inflight.delete(ip);
      if (ctx.aborted.aborted) return;
      countries.value = { ...countries.peek(), [ip]: code };
    });
  }

  // One sweep on open, so a pane opened between ticks is not blank for up to a
  // second. The recurring sweep belongs to the session's sampler.
  void props.client.refresh_peer_ips();

  /*
    The one thing on this pane that does not arrive as a prop. Agent calls are
    not the session sampler's to report, and a log that appeared up to a second
    after the call would be the wrong pace for watching an agent work — so this
    subscribes to the store directly, the way the brand in the top bar does.
  */
  const agent = signal<AgentActivity>(agentActivity());
  using _agent = disposable(
    subscribeAgentActivity((next) => (agent.value = next)),
  );

  using _keys = listen(window, "keydown", (event: Event) => {
    const keyEvent = event as KeyboardEvent;
    if (keyEvent.key === "Escape") {
      keyEvent.preventDefault();
      props.onClose();
    }
  });

  yield () => {
    // Read, not used: this is the dependency that repaints the pane each time
    // the session samples.
    props.tick.value;
    const countryMap = countries.value;
    const info = readInfo(props.client);
    // Absent until the session sampler has taken its first reading.
    const link = info?.transfer.link ?? null;
    const reach = info?.trackers.producer_reach;
    const reachLine = reach
      ? [
          reach.mdns ? "mdns" : null,
          reach.dht ? "dht" : null,
          `relay=${reach.relay}`,
        ]
          .filter(Boolean)
          .join(" · ")
      : "—";
    const relays = info?.trackers.relays ?? [];

    const held = props.held.value;
    const coverage = props.coverage.value;
    const files = props.files;
    const progress = shareProgress(files, held, coverage);
    const history = props.history.value;

    // The manifest's file count, which the host already knows — a slot is a
    // file, so the grid has a width even before any peer publishes anything.
    const totalSlots = files.length;
    // Sorted once, here, because everything below reads from it. The wasm
    // client's own order is hash-derived and reshuffles on every poll — see
    // `peers.ts`.
    const peers = sortPeers(info?.swarm.peers ?? []);
    const ourTree = peers.find((peer) => peer.role === "self")?.tree ?? null;
    const availabilities = peers.map((peer) =>
      peerAvailability(peer.id, peer.serving, peer.tree, totalSlots),
    );
    const gaps = totalSlots > 0 ? missingSlots(availabilities, totalSlots) : [];
    /** How many visible peers can serve each slot — 0 is a slot nobody has. */
    const swarmCoverage =
      totalSlots > 0 ? slotCoverage(availabilities, totalSlots) : [];
    /** How much of each slot *this tab* holds, in manifest order. */
    const heldStates = files.map((file) => fileSeedState(file, held, coverage));

    const agentActivityNow = agent.value;
    const callRows = agentActivityNow.log.map((call) => ({
      ...call,
      time: clockLabel(call.startedAt),
      // A call still running has no duration yet, and a 0 there would read as
      // one that finished instantly.
      ms: call.endedAt === null ? "—" : `${call.endedAt - call.startedAt}ms`,
    }));

    const peerRows = peers.map((peer, index) => {
      if (peer.ip) requestCountry(peer.ip);
      return {
        ...peer,
        availability: availabilities[index] as PeerAvailability,
        ipLabel: formatIpWithFlag(peer.ip, {
          countryCode: peer.ip ? (countryMap[peer.ip] ?? null) : null,
          kind: peer.ip_kind,
        }),
        // Full published label, e.g. `agent-share v0.1.0 (chrome, webrtc)`.
        clientLabel: peer.client,
      };
    });

    return (
      <div
        /*
          The one pane in the app that exists to be copied out of — fingerprints,
          relay URLs, peer addresses, fallback reasons. Selection is off
          app-wide (see `app.css`); this opts the whole surface back in.
        */
        class="selectable"
        style={{
          flex: 1,
          minHeight: 0,
          overflowY: "auto",
          // A row of air under the top bar, so the first panel's fill does not
          // read as an extension of the chrome above it.
          padding: "var(--ms-row) 2ch calc(2 * var(--ms-row))",
          background: t.bg,
        }}
      >
        <Bento>
          <Panel title="Activity">
            <Stack direction="column" gap={1}>
              <Stack direction="column" gap={1}>
                {/*
                  A row of space between the two graphs, and between them and the
                  numbers. `█` fills its whole line box, so on adjacent rows a
                  busy download and a busy upload meet in the middle and read as
                  one bar twice as tall.
                */}
                <Spark
                  glyph="↓"
                  label="download"
                  values={history.map((rate) => rate.down)}
                />
                <Spark
                  glyph="↑"
                  label="upload"
                  values={history.map((rate) => rate.up)}
                />
                <Stack direction="column" gap={0}>
                  <Text>
                    down {formatRate(link?.total.down_bps ?? 0)} · up{" "}
                    {formatRate(link?.total.up_bps ?? 0)}
                  </Text>
                  <Text>
                    received {humanBytes(link?.total.received ?? 0)} · sent{" "}
                    {humanBytes(link?.total.sent ?? 0)} · wire ratio{" "}
                    {ratioLabel(link)}
                  </Text>
                  <Text>
                    opened {clockLabel(props.openedAt)} · last activity{" "}
                    {clockLabel(props.lastActivityAt.value)} · connected{" "}
                    {formatDuration(info?.general.connected_ms_ui ?? 0)}
                  </Text>
                </Stack>
              </Stack>
              {/*
                This tab's own holdings, byte-weighted. The peer rows count slots
                because that is all a peer can answer for; here we know the sizes,
                and "3 of 8 files" on a share that is one video and seven READMEs
                would be a lie by arithmetic.
              */}
              <Stack direction="column" gap={0}>
                <Stack direction="row" gap={1}>
                  <ProgressBar
                    value={progress.fraction}
                    width={24}
                    showValue
                    tone={progress.fraction >= 1 ? "success" : "accent"}
                    label="share held by this tab"
                  />
                  <Text>
                    {humanBytes(progress.bytesHeld)} of{" "}
                    {humanBytes(progress.bytesTotal)}
                  </Text>
                </Stack>
                <Text color="fgMuted">
                  {progress.filesComplete}/{totalSlots} slots held whole
                </Text>
              </Stack>
            </Stack>
          </Panel>

          {/*
            Two squares of the same fixed size, side by side — the pair a
            BitTorrent client draws as Progress and Available. Fixed, because a
            square per slot would make the panel's height a function of the
            manifest, and a share of ten thousand files would push everything
            below it off the page. See `availabilityGrid`.
          */}
          <Panel title="Availability">
            <Stack direction="row" gap={4} wrap>
              <Stack direction="column" gap={0}>
                <SlotGrid
                  columns={GRID_SIDE}
                  states={resampleSlots(
                    totalSlots,
                    CELLS,
                    (slot) => heldStates[slot] ?? "none",
                  )}
                />
                <Text color="fgMuted">
                  held · {progress.filesComplete}/{totalSlots}
                </Text>
              </Stack>
              <Stack direction="column" gap={0}>
                <SlotGrid
                  columns={GRID_SIDE}
                  states={resampleSlots(totalSlots, CELLS, (slot) =>
                    (swarmCoverage[slot] ?? 0) > 0 ? "full" : "none",
                  )}
                />
                {/*
                  The one thing this view can tell you that a peer list cannot:
                  which parts of the share nobody visible still holds. Once the
                  origin is gone those slots are lost until somebody who has them
                  reappears.
                */}
                <Text color={gaps.length === 0 ? "fgMuted" : "warning"}>
                  available · {totalSlots - gaps.length}/{totalSlots}
                </Text>
              </Stack>
              {isCoarse(totalSlots, CELLS) ? (
                <Text color="fgMuted">one square is several files</Text>
              ) : null}
            </Stack>
          </Panel>

          <Panel title="Session">
            <Stack direction="column" gap={0}>
              <Text>
                transport {dash(info?.general.transport)} · status{" "}
                {props.status}
                {info?.general.mesh_up ? " · mesh up" : " · mesh down"}
              </Text>
              <Text>
                {totalSlots} files · {humanBytes(progress.bytesTotal)}
              </Text>
              <Text>nickname {dash(info?.general.nickname)}</Text>
              <Text>
                fingerprint {dash(info?.general.identity_fingerprint)}
              </Text>
            </Stack>
          </Panel>

          <Panel title="Relays">
            <Stack direction="column" gap={0}>
              {relays.length === 0 ? (
                <Text color="fgMuted">no relays configured</Text>
              ) : (
                /*
                  The whole ladder, not just the rung in use. "Which relay am I
                  on" is only half the question; the other half is what the
                  alternatives were, and a list of one cannot answer it.
                */
                relays.map((relay) => (
                  <Stack key={relay.url} direction="column" gap={0}>
                    <Stack direction="row" gap={1}>
                      {/*
                        Green for the one carrying traffic, grey for every rung
                        that is merely available. Two states rather than three:
                        a rung iroh never homed on has nothing to report, and
                        drawing that as an error would cry wolf on a ladder that
                        is working exactly as designed.
                      */}
                      <StatusDot status={relay.connected ? "ready" : "queued"}>
                        {relay.url}
                      </StatusDot>
                      {/*
                        Only when the picked relay is *not* the connected one —
                        the usual case is both at once, where the green dot has
                        already said it and a badge would be noise.
                      */}
                      {relay.home && !relay.connected ? (
                        <Badge tone="warning" variant="outline">
                          home
                        </Badge>
                      ) : null}
                    </Stack>
                    {relay.last_error ? (
                      <Text color="danger">{relay.last_error}</Text>
                    ) : null}
                  </Stack>
                ))
              )}
              {/*
                The ticket's declared reach, not a live measurement — these say
                which lookups this share *permits*, which is why they are labelled
                as the producer's rather than as this tab's.
              */}
              <Text color="fgMuted">producer reach {reachLine}</Text>
            </Stack>
          </Panel>

          <Panel title="Transfer">
            <Stack direction="column" gap={0}>
              <Text>
                mode {dash(info?.transfer.mount_mode)} → path{" "}
                {dash(info?.transfer.mount_path)}
              </Text>
              <Text>
                paths{" "}
                {(info?.transfer.mount_paths.length ?? 0) > 0
                  ? info!.transfer.mount_paths.join(", ")
                  : "—"}{" "}
                · mounted {props.mounted ? "yes" : "no"}
              </Text>
              <Text>{capabilitiesLine()}</Text>
              {props.mountError ? (
                <Text color="warning">last error: {props.mountError}</Text>
              ) : null}
              {/*
                The one line that answers "why is this on the relay?". Without it
                the fallback is a console warning nobody reading the pane can see.
              */}
              {info?.transfer.mount_fallback_reason ? (
                <Text color="warning">
                  fell back: {info.transfer.mount_fallback_reason}
                </Text>
              ) : null}
              {/*
                A stale tab looks exactly like a slow one until you can compare
                versions, and "who is feeding this" is the other half of it — a
                tree relayed by a seeder moves only when that seeder's does.
              */}
              {info ? (
                <Text color="fgMuted">
                  tree v{info.transfer.manifest_version} via {info.transfer.source}
                </Text>
              ) : null}
              {/*
                The mount connection's own counters, which — unlike the per-peer
                rows above — answer on the relay path too: they come from the QUIC
                state machine rather than a candidate pair. On a relay mount the
                peer rows can show a kilobyte of mesh chatter while megabytes of
                share moved right here, so this line is the one that reconciles.

                Wire bytes, and a smaller unit than the peer rows: QUIC sits below
                DTLS/SCTP on the WebRTC lane and below the relay's framing on the
                other. The two are not addable, which is why they are separate
                lines rather than one total.
              */}
              {link ? (
                <>
                  <Text>
                    mount wire: received {humanBytes(link.total.received)} ·
                    sent {humanBytes(link.total.sent)}
                  </Text>
                  <Text color="fgMuted">
                    by path: {laneSummary(link.lanes)} received
                  </Text>
                </>
              ) : null}
            </Stack>
          </Panel>

          {/*
            What an agent has done to this page.

            Present even where WebMCP is not, which is nearly everywhere — no
            browser enables it by default. A panel that disappeared would leave
            the reader unable to tell "nothing has called these tools" from
            "this browser cannot publish them", and those are opposite answers.
          */}
          <Panel title="WebMCP">
            <Stack direction="column" gap={1}>
              <Text color="fgMuted">
                {agentActivityNow.registered.length === 0
                  ? "no tools published — this browser has no WebMCP"
                  : `${agentActivityNow.registered.length} tools published · ${callsLabel(agentActivityNow.calls)}`}
              </Text>
              {/*
                Newest first, so the box needs no auto-scroll: a new line lands
                at the top, where the reader already is, instead of below the
                fold — and nothing jumps while an older line is being read.
              */}
              <div style={{ height: rows(LOG_ROWS), overflowY: "auto" }}>
                {callRows.length === 0 ? (
                  <Text color="fgMuted">no calls yet</Text>
                ) : (
                  <Table
                    rows={callRows}
                    rowKey={(call) => call.id}
                    columns={[
                      { key: "time", header: "time", width: 11 },
                      { key: "tool", header: "tool", width: 20 },
                      {
                        /*
                          The one flexible column, and the only one allowed to
                          be. Everything else has a fixed width, so the table
                          fits a half-width panel and the arguments give up
                          their tail rather than the panel scrolling sideways.
                        */
                        key: "args",
                        header: "args",
                        render: (call) => (
                          <Text
                            color="fgMuted"
                            truncate
                            title={call.args || undefined}
                          >
                            {dash(call.args)}
                          </Text>
                        ),
                      },
                      {
                        key: "outcome",
                        header: "result",
                        width: 12,
                        // The code is what fits; the prose behind it is the
                        // hover, since a failure message is a sentence. The
                        // code repeats in the hover because the longest ones
                        // are exactly the ones the column clips.
                        render: (call) =>
                          call.endedAt === null ? (
                            <Text color="fgMuted">…running</Text>
                          ) : call.outcome === "ok" ? (
                            <Text color="success">ok</Text>
                          ) : (
                            <Text
                              color="warning"
                              truncate
                              title={
                                call.error
                                  ? `${call.outcome} — ${call.error}`
                                  : call.outcome
                              }
                            >
                              {call.outcome}
                            </Text>
                          ),
                      },
                      { key: "ms", header: "took", width: 8, align: "right" },
                    ]}
                  />
                )}
              </div>
            </Stack>
          </Panel>

          <Panel title="Peers" wide>
            {peerRows.length === 0 ? (
              <Text color="fgMuted">no peers</Text>
            ) : (
              <div>
                {Style(PEER_TABLE)}
                <Table
                  rows={peerRows}
                  /*
                    A floor, because `Table` is `width: 100%` and its one
                    flexible column absorbs whatever is left. Without this the
                    client column is what a narrow window takes the space from,
                    and it silently shrinks to nothing rather than the table
                    scrolling — the column vanishes and nothing says so.
                  */
                  style={{ minWidth: "150ch" }}
                  rowKey={(peer) => peer.id}
                  /*
                    Left-aligned columns first, right-aligned ones last, and the
                    order is load-bearing rather than tidy: a right-aligned cell
                    pins its content to its own right edge and a left-aligned one
                    to its left, so the single cell of column gap between them
                    disappears and two headers read as one word. Keeping the
                    numeric block at the end leaves exactly one such boundary,
                    where `id` is short enough to leave air.
                  */
                  columns={[
                    { key: "ipLabel", header: "ip", width: 24 },
                    {
                      key: "clientLabel",
                      header: "client",
                      render: (peer) => (
                        <Text truncate>{peer.clientLabel}</Text>
                      ),
                    },
                    { key: "proto", header: "proto", width: 9 },
                    {
                      key: "flags",
                      header: "flags",
                      width: 6,
                      // Empty flags mean gossip-only, which the dash says
                      // without implying the field failed to load.
                      render: (peer) => dash(peer.flags),
                    },
                    {
                      key: "serving",
                      header: "slots",
                      width: 16,
                      render: (peer) => (
                        <PeerSlots
                          peer={peer.availability}
                          total={totalSlots}
                          ourTree={ourTree}
                        />
                      ),
                    },
                    {
                      key: "id",
                      header: "id",
                      width: 19,
                      render: (peer) => (
                        <Text color="fgMuted">{shortId(peer.id)}</Text>
                      ),
                    },
                    {
                      key: "rtt_ms",
                      header: "ping",
                      width: 8,
                      align: "right",
                      render: (peer) => pingLabel(peer.rtt_ms),
                    },
                    /*
                      Rates and byte counts only for peers we hold a data channel
                      with. A gossip-only row has no candidate pair, so a zero
                      there would read as "sent nothing" rather than
                      "not measured" — which is why `measured` gates all four
                      columns rather than each formatter dashing on its own.
                    */
                    {
                      key: "up_bps",
                      header: "up",
                      width: 10,
                      align: "right",
                      render: (peer) =>
                        measured(peer) ? formatRate(peer.up_bps) : "—",
                    },
                    {
                      key: "down_bps",
                      header: "down",
                      width: 10,
                      align: "right",
                      render: (peer) =>
                        measured(peer) ? formatRate(peer.down_bps) : "—",
                    },
                    {
                      key: "bytes_sent",
                      header: "sent",
                      width: 10,
                      align: "right",
                      render: (peer) =>
                        measured(peer) ? humanBytes(peer.bytes_sent) : "—",
                    },
                    {
                      key: "bytes_received",
                      header: "received",
                      width: 10,
                      align: "right",
                      render: (peer) =>
                        measured(peer) ? humanBytes(peer.bytes_received) : "—",
                    },
                  ]}
                />
              </div>
            )}
          </Panel>

          {/*
            Behind `?dev=true`, and off for anyone handed a share link.

            Killing the connection is the only way to rehearse recovery on
            demand: the real failure — a backgrounded tab whose timers stretch
            past the keep-alive interval — happens only sometimes, so testing
            the reconnect used to mean idling a tab for minutes and hoping.

            Nothing here re-dials. Recovery is left to the ordinary triggers
            (press Download, or leave and return to the tab), because those are
            the paths worth testing and a self-healing button would skip them.
            So clicking this looks like it does nothing, which is the point.
          */}
          {props.dev ? (
            <Panel title="Dev">
              <Stack direction="row" gap={1}>
                <Button
                  variant="secondary"
                  onclick={props.onKillConnection}
                  disabled={props.killDisabled}
                >
                  Kill connection
                </Button>
              </Stack>
              <Text>
                Closes the mount connection. Nothing visible happens until you
                press Download or leave and return to this tab — that is what
                triggers the reconnect.
              </Text>
            </Panel>
          ) : null}
        </Bento>
      </div>
    );
  };
});

/** Whether this peer has a candidate pair to read counters off at all. */
function measured(peer: {
  bytes_sent: number;
  bytes_received: number;
}): boolean {
  return peer.bytes_sent > 0 || peer.bytes_received > 0;
}
