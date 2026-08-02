# Batch 1 bench results

Native↔native `agent-share bench --transport webrtc` on one machine
(loopback + local STUN-gathered candidates), macOS, release build, 10 s
windows, 3 runs per condition. Raw JSON in this directory.

Changes between the two conditions (research doc recommendations 2, 3, 6, 7):
data channel negotiated **unreliable + unordered** (`maxRetransmits: 0`)
instead of default reliable+ordered; browser hub session-leak fix; TURN
credential cache + fetch timeout; drop-counter logging. Only the channel
config affects this native bench path.

| Run | Baseline MiB/s | After MiB/s | Baseline p95 ms | After p95 ms | Baseline connect ms | After connect ms |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 104.6 | 101.9 | 0.309 | 0.198 | 110.2 | 91.2 |
| 2 | 64.5 | 102.8 | 0.179 | 0.192 | 78.0 | 54.6 |
| 3 | 66.2 | 100.5 | 0.324 | 0.291 | 58.1 | 66.9 |
| **median** | **66.2** | **101.9** | 0.309 | 0.198 | 78.0 | 66.9 |

Reading:

- **Throughput: +54% on the median run, and the variance collapsed** (64–105
  → 100–103 MiB/s). With reliable+ordered SCTP under QUIC, SCTP's own
  retransmission/ordering machinery interfered unpredictably with QUIC's
  congestion controller even on a clean loopback; as a plain datagram pipe
  the lane sits stably at the ~100 MiB/s the QUIC layer can drive.
- **Latency medians steadied** (0.080–0.147 → 0.081–0.088 ms); p95 similar.
- **connect_ms is signaling + ICE dominated** and unchanged within noise, as
  expected — the TURN cache only affects *browser* connects (native has no
  TURN client), and this bench reconnects with a fresh process each run.
- The double-congestion-control pathology this change removes grows with RTT
  and loss; loopback is its *best* case, so WAN gains should be larger. A
  lossy-link run (dnctl/pfctl) was not performed in this pass.

Not measured here: the browser leg (`web/lab.html`). The session-leak fix is
behavioral (reconnect to a browser producer now succeeds; before it failed
with "a live WebRTC session already exists") and the TURN cache means at most
one credential fetch per tab per ~TTL instead of one per connect.
