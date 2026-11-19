// When to try the daemon again, and when a connection that looks open has gone dead.
// A sleeping PC or a dropped VPN often leaves a socket that never reports closing.

const BASE_MS = 2000;
const MAX_MS = 15000;

/** Wait before retry number `attempt` (0 is the first): 2 s, then slower, never past 15 s. */
export function retryDelay(attempt: number): number {
  return Math.min(MAX_MS, Math.round(BASE_MS * 1.6 ** attempt));
}

/** The daemon sends a heartbeat to remote devices this often. */
export const HEARTBEAT_MS = 15000;
/** Three missed heartbeats and the link is treated as dead. */
export const DEAD_AFTER_MS = HEARTBEAT_MS * 3;

/** Whether nothing has arrived for too long to trust the socket. */
export function isDead(lastHeard: number, now: number, limit = DEAD_AFTER_MS): boolean {
  return now - lastHeard > limit;
}
