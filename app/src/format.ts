// ブロックの見出しに出す時刻と実行時間。

/** 時刻を `21:05` の形にする。 */
export function formatClock(epochMs: number): string {
  const date = new Date(epochMs);
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 実行時間を `0.3s` / `1m 05s` / `1h 50m` の形にする。 */
export function formatDuration(ms: number): string {
  const seconds = Math.max(0, ms) / 1000;
  if (seconds < 59.95) {
    return `${seconds.toFixed(1)}s`;
  }
  const whole = Math.round(seconds);
  if (whole < 3600) {
    return `${Math.floor(whole / 60)}m ${pad(whole % 60)}s`;
  }
  return `${Math.floor(whole / 3600)}h ${pad(Math.floor((whole % 3600) / 60))}m`;
}

function pad(value: number): string {
  return value < 10 ? `0${value}` : String(value);
}

/** ホームディレクトリを `~` にして短くする。 */
export function shortenPath(path: string, home: string | null): string {
  if (home && (path === home || path.startsWith(`${home}/`))) {
    return `~${path.slice(home.length)}`;
  }
  return path;
}
