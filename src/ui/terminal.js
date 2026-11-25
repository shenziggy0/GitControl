import { Channel, invoke } from '@tauri-apps/api/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

const sessions = new Map();

function terminalOptions() {
  const dark=document.documentElement.dataset.theme==='dark';
  const style=getComputedStyle(document.documentElement);
  const color=name=>style.getPropertyValue('--'+name).trim();
  // Keep xterm's dark ANSI palette. The light palette also covers apps that emit bright ANSI codes.
  const ansi = dark ? {} : {
    black:color('terminal-ansi-black'), red:color('terminal-ansi-red'), green:color('terminal-ansi-green'),
    yellow:color('terminal-ansi-yellow'), blue:color('terminal-ansi-blue'), magenta:color('terminal-ansi-magenta'),
    cyan:color('terminal-ansi-cyan'), brightBlack:color('terminal-bright-black'), brightRed:color('terminal-bright-red'),
    brightGreen:color('terminal-bright-green'), brightYellow:color('terminal-ansi-yellow'), brightBlue:color('terminal-bright-blue'),
    brightMagenta:color('terminal-bright-magenta'), brightCyan:color('terminal-bright-cyan')
  };
  return {
    cursorBlink: !matchMedia('(prefers-reduced-motion: reduce)').matches,
    minimumContrastRatio: dark ? 1 : 4.5,
    theme: {
      background:color('terminal'), foreground:color('ink'), cursor:color('terminal-cursor'),
      selectionBackground:color('terminal-selection'), ...ansi
    }
  };
}

export function updateTerminalAppearance() {
  const options=terminalOptions();
  for (const {term} of sessions.values()) {
    term.options=options;
  }
}

export async function showTerminal(projectId, enabled, container, report, canFocus) {
  for (const [id, item] of sessions) item.element.hidden = id !== projectId || !enabled;
  if (!enabled || !projectId) return;
  const existing = sessions.get(projectId);
  if (existing) {
    existing.fit.fit();
    if (canFocus()) existing.term.focus();
    return;
  }
  const element = document.createElement('div');
  element.className = 'terminal-surface';
  container.append(element);
  const term = new Terminal({
    fontFamily: 'Menlo, monospace', fontSize: 13, scrollback: 4000,
    ...terminalOptions()
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(element);
  fit.fit();
  const entry = { term, element, fit, id: null, observer: null };
  sessions.set(projectId, entry);
  const events = new Channel();
  events.onmessage = event => {
    if (sessions.get(projectId) !== entry) return;
    if (event.kind === 'output') {
      term.write(Uint8Array.from(atob(event.data), c => c.charCodeAt(0)));
    } else {
      term.writeln('\r\n[终端已退出 · '+(event.exit ?? '未知状态')+'，关闭后可重新打开]');
    }
  };
  try {
    const id = await invoke('terminal_open', { projectId, rows: term.rows, cols: term.cols, events });
    if (sessions.get(projectId) !== entry) {
      await invoke('terminal_close',{projectId,id});
      return;
    }
    entry.id = id;
    term.onData(data => invoke('terminal_write',{projectId,id,data}).catch(report));
    term.onResize(({rows, cols}) => invoke('terminal_resize',{projectId,id,rows,cols}).catch(report));
    entry.observer = new ResizeObserver(() => { if (!element.hidden && element.offsetWidth) fit.fit(); });
    entry.observer.observe(element);
    if (!element.hidden&&canFocus()) term.focus();
  } catch (error) {
    if (sessions.get(projectId) === entry) await closeTerminal(projectId);
    report(error);
  }
}

export async function closeTerminal(projectId) {
  const entry = sessions.get(projectId);
  if (!entry) return;
  sessions.delete(projectId);
  entry.observer?.disconnect();
  entry.term.dispose();
  entry.element.remove();
  if (entry.id) await invoke('terminal_close',{projectId,id:entry.id});
}
