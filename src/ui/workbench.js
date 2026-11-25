import { Channel, invoke } from '@tauri-apps/api/core';

export function workbenchSession() {
  return {
    commitDraft: '', commitError: '', commitBusy: false, commitRequest: null, lastCommit: null,
    rightDiff: null, summaries: {}, summaryError: '', sync: null,
    snapshots: null, snapshotList: null, snapshotName: ''
  };
}

const repoKey = repository => repository.projectId || repository.parentProjectId + '/' + repository.relativePath;
const syncLabels = { local: '仅本机', no_upstream: '未关联上游', unknown: '待核对' };

async function limited(items, visit) {
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(3, items.length) }, async () => {
    while (next < items.length) await visit(items[next++]);
  }));
}

export function workbench(compactLayout) {
  let searchVersion = 0;
  let searchTimer;


  return {
    searchScope: 'project', crossHits: [], crossBusy: false, crossFailures: 0,
    compactWorkbench: compactLayout.matches,
    get selectedText() {
      const count = new Set(this.s.selected.map(file => file.path)).size;
      return count + ' 个文件' + (count !== this.s.selected.length ? ' · ' + this.s.selected.length + ' 组改动' : '');
    },
    get rightPreview() {
      return !this.compactWorkbench && this.s.view === 'status' ? this.s.rightDiff : null;
    },
    get syncStatus() {
      if (!this.s.repo?.remotes.length) return '这个仓库目前只保存在本机';
      if (!this.current?.remote) return '当前分支尚未关联上游';
      const remote = this.s.remotes[this.current.remote];
      if (this.current.remote === '.') return '上游是本地分支';
      if (remote?.error) return '远端核对失败 · 点击刷新重试';
      if (this.current.gone) return '远端上游分支已不存在';
      if (!this.current.counts) return '正在核对或缺少远端历史';
      return '';
    },
    get childrenWithSummary() {
      const rows = this.s.discovery?.result?.repositories || [];
      const score = row => {
        const summary = this.s.summaries[row.relativePath];
        return Number(!!(summary?.files || summary?.ahead));
      };
      return [...rows].sort((a, b) => score(b) - score(a) || a.relativePath.localeCompare(b.relativePath));
    },
    summaryText(summary) {
      if (!summary) return '读取状态中…';
      if (summary.error) return '无法读取状态';
      const sync = summary.ahead == null ? syncLabels[summary.source] || '待核对'
        : summary.source === 'verified'
          ? summary.ahead + ' 次待推送 · ' + this.shortDate(summary.timestamp)
          : summary.ahead + ' 次待推送 · 按本地记录';
      return [summary.branch || '分离 HEAD', summary.files + ' 个改动文件', sync].join(' · ');
    },
    shortDate(timestamp) {
      return timestamp == null ? '' : new Date(timestamp * 1000).toLocaleString('zh-CN', {hour12:false});
    },
    bytesLabel(bytes) {
      return bytes < 1048576 ? Math.ceil(bytes / 1024) + ' KB' : (bytes / 1048576).toFixed(1) + ' MB';
    },
    async refreshSummaries(id, repositories) {
      const state = this.sessions[id];
      if (!state) return;
      const token = crypto.randomUUID();
      state.summaryToken = token;
      state.summaries = {};
      const current = () => this.sessions[id] === state && state.summaryToken === token;
      await limited(repositories, async child => {
        if (!current()) return;
        try {
          const summary = await invoke('project_child_summary', { parentProjectId:id, relativePath:child.relativePath });
          if (current()) state.summaries[child.relativePath] = summary;
        } catch (error) {
          if (current()) state.summaries[child.relativePath] = { error:String(error) };
        }
      });
    },
    openChildChanges(path) {
      return this.openChild(this.projectId, path, () => {
        this.showStatus();
        this.$nextTick(() => {
          this.$refs.changes.scrollIntoView({ block:'nearest' });
          this.$refs.changes.focus({ preventScroll:true });
        });
      });
    },
    openChildSync(path) {
      return this.openChild(this.projectId, path, () => {this.showStatus(); this.openSync(true);});
    },
    async submitSelected() {
      const state = this.s;
      const id = this.projectId;
      if (state.commitBusy || state.commitUncertain || !state.selected.length || !state.commitDraft.trim()) return;
      const payload = {
        projectId:id, branch:state.repo?.branch, head:state.repo?.head,
        selected:state.selected.map(change => ({...change})), message:state.commitDraft.trim()
      };
      const signature = JSON.stringify(payload);
      if (state.commitRequest?.signature !== signature) {
        state.commitRequest = { signature, id:crypto.randomUUID() };
      }
      state.commitBusy = true;
      state.commitError = '';
      try {
        const oid = await invoke('repo_commit_selected', { ...payload, requestId:state.commitRequest.id });
        state.lastCommit = oid;
        state.commitDraft = '';
        state.commitRequest = null;
        const captured = new Set(payload.selected.map(file => file.group + '\0' + file.path));
        state.selected = state.selected.filter(file => !captured.has(file.group + '\0' + file.path));
        await this.refresh(id, false);
      } catch (error) {
        state.commitError = String(error);
        if (/待核查|已生成/.test(state.commitError)) state.commitUncertain = true;
      } finally { state.commitBusy = false; }
    },
    async openCommitOid(oid) {
      const id = this.projectId;
      try {
        const commit = this.s.all.find(commit => commit.sha === oid)
          || await invoke('repo_commit_summary', {projectId:id, oid});
        if (this.projectId === id) await this.inspect(commit);
      } catch (error) { if (this.projectId === id) this.report(error); }
    },
    async recheckCommit() {
      const state = this.s;
      const id = this.projectId;
      const expected = state.commitRequest?.id;
      if (!expected || state.commitBusy) return;
      state.commitBusy = true;
      try {
        const oid = await invoke('repo_commit_outcome', {projectId:id, requestId:expected});
        await this.refresh(id, false);
        if (oid) {
          state.lastCommit = oid;
          state.commitDraft = '';
          state.selected = [];
          state.commitRequest = null;
          state.commitUncertain = false;
          state.commitError = '';
        }
      } catch (error) { state.commitError = String(error); }
      finally { state.commitBusy = false; }
    },
    async suggestCommitMessage() {
      const state = this.s;
      const id = this.projectId;
      if (!state.selected.length || state.running || state.commitBusy) return;
      const startingDraft = state.commitDraft;
      const startingSelection = JSON.stringify(state.selected);
      const requestId = crypto.randomUUID();
      const events = new Channel();
      let draft = '';
      events.onmessage = event => { if (event.kind === 'delta') draft += event.text; };
      state.running = requestId;
      state.commitError = '';
      try {
        await invoke('agent_send', {
          projectId:id, requestId, events, purpose:'commit_message', commitOid:null,
          question:'根据所选差异写一条简洁中文 Git 提交说明。只输出一行说明。',
          selected:state.selected.map(change => ({...change}))
        });
        if (state.commitDraft === startingDraft && JSON.stringify(state.selected) === startingSelection) {
          const line = draft.trim().split('\n')[0]?.replace(/^[`「"“]+|[`」"”]+$/g, '');
          if (line && /[\u3400-\u9fff]/.test(line)) state.commitDraft = line;
          else state.commitError = '未收到可用的中文说明，可以直接输入。';
        }
      } catch (error) { state.commitError = String(error); }
      finally { state.running = null; }
    },
    currentPatchChange(detail) {
      return detail?.file && !detail.oid && this.s.repo?.head === detail.head
        ? this.s.repo?.files.find(file => file.path === detail.file.path && file.group === detail.file.group && file.stamp === detail.file.stamp)
        : null;
    },
    async loadDiff(detail) {
      try {
        detail.patch = await invoke('repo_patch', {
          projectId:this.projectId, oid:detail.oid, path:detail.file.path, group:detail.file.group || '',
          stamp:detail.file.stamp ?? null, structured:true
        });
      } catch (error) { detail.error = String(error); }
      finally { detail.pending = false; }
    },
    nextDiff(detail) {
      const list = this.changeGroups.flatMap(group => group.files);
      const index = list.findIndex(file => file.path === detail.file.path && file.group === detail.file.group);
      if (index < 0 || index >= list.length - 1) return;
      if (this.detailOpen && this.detailView === detail) this.backFromDetail();
      this.viewPatch(list[index + 1], null);
    },
    hasNextDiff(detail) {
      if (!detail?.file || detail.oid) return false;
      const list = this.changeGroups.flatMap(group => group.files);
      const index = list.findIndex(file => file.path === detail?.file.path && file.group === detail?.file.group);
      return index >= 0 && index < list.length - 1;
    },
    async lookupLine(detail, line, oldSide) {
      const number = oldSide ? line.old : line.new;
      detail.lookup = { pending:true, error:'', value:null, line, oldSide };
      const lookup = detail.lookup;
      try {
        lookup.value = await invoke('repo_line_origin', {
          projectId:this.projectId, selection:{
            oid:detail.oid, path:detail.file.path, group:detail.file.group || '', stamp:detail.file.stamp ?? null,
            head:detail.head, oldSide, number, text:line.text
          }
        });
      } catch (error) { lookup.error = String(error); }
      finally { lookup.pending = false; }
    },
    explainLine(detail) {
      const lookup = detail.lookup;
      if (!lookup || lookup.pending || lookup.error) return;
      const path = lookup.oldSide ? detail.patch.oldPath : detail.patch.newPath;
      const number = lookup.oldSide ? lookup.line.old : lookup.line.new;
      this.openChat();
      this.s.chatCommit = lookup.value?.commit.sha || null;
      this.ask('请解释所选行的修改。区分提交说明中的事实和推测。\n文件：' + path +
        '\n行号：' + number + '\n所选内容：' + lookup.line.text, 'explain_line');
    },
    async openSync(pushing) {
      const id = this.projectId;
      const state = this.s;
      if (state.sync?.busy) return;
      state.sync = { pushing, pending:true, busy:false, error:'', result:null };
      const view = state.sync;
      this.$nextTick(() => { if (this.s === state) this.$refs.syncPanel?.scrollIntoView({block:'nearest'}); });
      try { view.result = await invoke('repo_sync_preview', { projectId:id, pushing }); }
      catch (error) { view.error = String(error); }
      finally { view.pending = false; }
    },
    async sendPush() {
      const id = this.projectId;
      const state = this.s;
      const view = state.sync;
      if (!view?.result?.preview.canPush || view.busy) return;
      view.busy = true;
      try {
        const oid = await invoke('repo_push', { projectId:id, token:view.result.token });
        view.success = '已核对远端包含 ' + this.short(oid);
        await this.refresh(id, true);
      } catch (error) { view.error = String(error); }
      finally {
        view.result.preview.canPush = false;
        view.busy = false;
      }
    },
    changeSearchScope(scope) {
      this.searchScope = scope;
      this.selectedSearchKey = null;
      this.scheduleSearch();
      this.$refs.search.focus();
    },
    scheduleSearch() {
      clearTimeout(searchTimer);
      const version = ++searchVersion;
      this.crossHits = [];
      this.crossBusy = false;
      this.crossFailures = 0;
      const query = this.query.trim();
      if (!this.palette || this.searchScope !== 'all' || !query) return;
      this.crossBusy = true;
      searchTimer = setTimeout(() => this.runCrossSearch(version, query), 180);
    },
    async runCrossSearch(version, query) {
      const matches = new Map();
      for (const project of this.projects) {
        const session = this.sessions[project.id];
        if (session?.repo?.kind !== 'folder') matches.set(project.path, { projectId:project.id });
        for (const child of session?.discovery?.result?.repositories || []) {
          const path = project.path.replace(/\/$/, '') + '/' + child.relativePath;
          if (!matches.has(path)) matches.set(path, { parentProjectId:project.id, relativePath:child.relativePath });
        }
      }
      const current = () => this.palette && this.searchScope === 'all' && searchVersion === version;
      const results = [];
      let failures = 0;
      await limited([...matches.values()], async repository => {
        if (!current()) return;
        try {
          const response = await invoke('workspace_search', { repository, query });
          for (const hit of response.hits) {
            const ref = hit.branch ? {...hit.branch, kind:hit.cloud ? 'remote' : 'local'} : null;
            const identity = hit.commit?.sha || ref?.kind + '/' + ref?.name;
            results.push({ key:repoKey(repository)+'/'+identity, repository, path:response.path,
              kind:response.label, location:this.searchLocation(response.path), label:hit.commit?.subject || ref?.name, commit:hit.commit, ref });
          }
        } catch { failures++; }
      });
      if (!current()) return;
      const queryLower = query.toLowerCase();
      const exact = hit => Number(!!hit.commit?.sha.startsWith(queryLower));
      results.sort((a,b) => exact(b)-exact(a) || a.path.localeCompare(b.path) || a.label.localeCompare(b.label));
      this.crossHits = results.slice(0, 12);
      this.crossFailures = failures;
      this.crossBusy = false;
    },
    searchLocation(path) {
      const parent = this.projects.filter(project => path.startsWith(project.path.replace(/\/$/, '')+'/'))
        .sort((a,b) => b.path.length-a.path.length)[0];
      return parent ? parent.name+'/'+path.slice(parent.path.replace(/\/$/, '').length+1) : path;
    },
    async openCrossHit(hit) {
      const show = () => {
        if (hit.commit) this.inspect(hit.commit);
        else if (hit.ref?.kind === 'remote' && !this.cloud.some(ref => ref.name === hit.ref.name)) this.openCommitOid(hit.ref.oid);
        else if (hit.ref) this.branchView(hit.ref, hit.ref.kind === 'remote');
      };
      const source = hit.repository;
      if (source.projectId) {
        const version = this.selectionVersion + 1;
        await this.selectProject(source.projectId, false);
        if (this.selectionVersion === version && this.projectId === source.projectId && this.active) show();
      } else {
        await this.openChild(source.parentProjectId, source.relativePath, show);
      }
    },
    async showSnapshots() {
      const state = this.s;
      state.snapshots = { busy:false, error:'', items:null, target:null, file:null, selected:null };
      this.$nextTick(() => { if (this.s === state) this.$refs.snapshotPanel?.scrollIntoView({block:'nearest'}); });
      await this.reloadSnapshots(state, this.projectId);
    },
    async reloadSnapshots(state, id) {
      const view = state.snapshots;
      if (!view) return;
      try { view.items = await invoke('snapshot_list', { projectId:id }); }
      catch (error) { view.error = String(error); }
    },
    async snapshotAction(command, args, after) {
      const state = this.s;
      const id = this.projectId;
      const view = state.snapshots;
      if (!view || view.busy) return;
      view.busy = true;
      view.error = '';
      try { await after(await invoke(command, {projectId:id, ...args}), view, state, id); }
      catch (error) { view.error = String(error); }
      finally { view.busy = false; }
    },
    createSnapshot() {
      return this.snapshotAction('snapshot_create', {name:this.s.snapshotName}, async (_, view, state, id) => {
        state.snapshotName = '';
        await this.reloadSnapshots(state, id);
      });
    },
    chooseSnapshot(item) {
      return this.snapshotAction('snapshot_preview', {snapshotId:item.id, path:null}, async (rows, view) => {
        view.target = item;
        view.rows = rows;
        view.selected = null;
        view.file = null;
        view.confirmDelete = false;
      });
    },
    previewSnapshotFile(row) {
      const view = this.s.snapshots;
      if (view.busy) return;
      const target = view.target;
      view.file = null;
      return this.snapshotAction('snapshot_preview', {snapshotId:target.id, path:row.path}, async (file, view) => {
        view.selected = row;
        view.file = file;
      });
    },
    restoreSnapshotFile() {
      const view = this.s.snapshots;
      return this.snapshotAction('snapshot_restore_file', {
        snapshotId:view.target.id, path:view.selected.path, version:view.file.version
      }, async (undo, view, state, id) => {
        view.undo = undo;
        view.selected = null;
        view.file = null;
        await this.reloadSnapshots(state, id);
        view.rows = await invoke('snapshot_preview', {projectId:id, snapshotId:view.target.id, path:null});
        await this.refresh(id, false);
      });
    },
    deleteSnapshot() {
      const target = this.s.snapshots.target;
      return this.snapshotAction('snapshot_delete', {snapshotId:target.id}, async (_, view, state, id) => {
        view.target = null;
        view.rows = [];
        view.file = null;
        view.selected = null;
        view.confirmDelete = false;
        await this.reloadSnapshots(state, id);
      });
    }
  };
}
