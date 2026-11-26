// Live updates from the Hub Durable Object. Change events are hints to refetch; the API
// stays the source of truth. Reconnects with backoff.
import { getToken } from './api';

export interface ChangeEvent {
  kind: string;
  id: string;
}

export function subscribe(onEvent: (e: ChangeEvent) => void): () => void {
  let ws: WebSocket | null = null;
  let closed = false;
  let delay = 1000;

  const connect = () => {
    const token = getToken();
    if (!token || closed) return;
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(`${proto}://${location.host}/hub?token=${encodeURIComponent(token)}`);
    ws.onopen = () => (delay = 1000);
    ws.onmessage = (m) => {
      try {
        onEvent(JSON.parse(m.data));
      } catch {
        /* ignore malformed */
      }
    };
    ws.onclose = () => {
      if (closed) return;
      setTimeout(connect, delay);
      delay = Math.min(delay * 2, 30000);
    };
  };

  connect();
  return () => {
    closed = true;
    ws?.close();
  };
}
