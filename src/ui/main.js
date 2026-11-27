import Alpine from 'alpinejs';
import { Channel, invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  createIcons, ArrowDownToLine, ArrowLeft, ArrowUp, ArrowUpFromLine, ArrowUpRight,
  ChevronLeft, ChevronRight, ChevronUp, CircleCheck, Cloud, CornerDownLeft, FileDiff, Files,
  FileText, FolderGit2, FolderOpen, GitBranch, GitCommitHorizontal, GitCompareArrows,
  GitGraph, GitPullRequest, History, Layers, LayoutDashboard, ListFilter, MessageSquare, Monitor, Moon, PanelLeft,
  Plus, RefreshCw, Search, Square, SquareTerminal, Sun, Tag, X
} from 'lucide';
import template from './workspace.html?raw';
import diffTemplate from './diff-panel.html?raw';
import { workbench, workbenchSession } from './workbench.js';
import { mountGraph } from './graph.js';
import { renderAgentMarkdown } from './markdown.js';
import { showTerminal, closeTerminal, updateTerminalAppearance } from './terminal.js';

const agentLoginTerminal = 'antigravity-login';
import './style.css';

const PAGE_SIZE = 60;
let selectionQueue = Promise.resolve();
let searchTrigger = null;
let searchExit = null;
let viewEntrance = null;
const systemDark = matchMedia('(prefers-color-scheme: dark)');
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
const compactLayout = matchMedia('(max-width: 960px)');
const appearanceKey = 'gitcontrol.appearance';
const groups = [
  {id:'staged', label:'已暂存'}, {id:'unstaged', label:'未暂存'},
  {id:'untracked', label:'未跟踪'}, {id:'conflict', label:'冲突'}
];

function session() {
  return {
    view:'status', detailTrail:[], statusScroll:0, chat:false, chatOrigin:null, chatCommit:null, term:false, messages:[], draft:'', running:null, selected:[],
    repo:null, discovery:null, childrenOpen:true, remotes:{}, counts:{}, error:'', refreshing:false, refreshingRemote:false, dirty:false, remoteDirty:false,
    all:[], revision:0, graphBusy:false, historyKey:null, headFiles:null, headFilesError:false, headFilesOpen:false,
    scope:'all', page:0, focusSha:null, focusSerial:0,
    ...workbenchSession()
  };
}

// A successful terminal push normally moves a local remote-tracking ref.
// Use that change to request fresh server data for the cloud labels.
function remoteStateChanged(previous, current) {
  if (previous?.kind!=='git') return true;
  return current.remotes.length!==previous.remotes.length ||
    current.remotes.some((name,i)=>name!==previous.remotes[i]) ||
    current.tracking.length!==previous.tracking.length ||
    current.tracking.some(({name,oid},i)=>name!==previous.tracking[i].name || oid!==previous.tracking[i].oid);
}

function branchPairs(s) {
  const cloud = Object.values(s.remotes).flatMap(r=>r.refs);
  const used = new Set();
  const rows = (s.repo?.local||[]).map(local => {
    const state = s.remotes[local.remote];
    const name = local.remote+'/'+local.merge.replace(/^refs\/heads\//,'');
    const remote = state?.refs.find(r=>r.name===name);
    if (remote) used.add(remote.name);
    return { key:'local/'+local.name, name:local.name, local:{...local,
      gone:!!local.merge&&!!state?.timestamp&&!state.error&&!remote,
      counts:s.counts[local.name]}, remote };
  });
  for (const remote of cloud) {
    if (!used.has(remote.name)) rows.push({key:'remote/'+remote.name,name:remote.name,local:null,remote});
  }
  return rows;
}

Alpine.data('workspace',()=>Object.defineProperties({
  projects:[], sessions:{}, projectId:null, empty:session(), sidebar:true, projectsOpen:true,
  branchesOpen:true, remotesOpen:false, tagsOpen:false, palette:false, query:'', notice:'',
  pane:'editor', appearance:'system', selectedSearchKey:null, loadingProjects:isTauri(),
  agentOptions:[], selectedAgent:'', scanningAgents:false, switchingAgent:false, lastAgentScan:0,
  agentLoginError:'', agentTerminalMode:false,
  renderAgentMarkdown,
  selectionVersion:0, openingChild:false,
  get project(){return this.projects.find(p=>p.id===this.projectId)},
  get selectedAgentOption(){return this.agentOptions.find(option=>option.kind===this.selectedAgent);},
  get s(){return this.sessions[this.projectId]||this.empty},
  get visiblePane(){return compactLayout.matches ? this.pane : 'editor';},
  get active(){return this.s.repo?.kind==='git'&&!this.s.error},
  get discoveryText(){
    const scan=this.s.discovery;
    if (!scan||scan.busy) return '正在扫描两层子目录…';
    if (scan.error) return scan.error;
    const {repositories,unreadableCount}=scan.result;
    if (unreadableCount) return `已发现 ${repositories.length} 个仓库，有 ${unreadableCount} 个目录未能检查。`;
    if (!repositories.length) return '已检查此文件夹下两层目录，发现 0 个 Git 仓库。';
    return `发现 ${repositories.length} 个仓库`;
  },
  get projectGroups(){
    const projectsByPath=new Map(this.projects.map(project=>[project.path,project]));
    const owners=new Map();
    for (const parent of this.projects) {
      const scan=this.sessions[parent.id]?.discovery?.result;
      for (const candidate of scan?.repositories||[]) {
        const path=parent.path.replace(/\/$/,'')+'/'+candidate.relativePath;
        const currentOwner=owners.get(path);
        if (!currentOwner||parent.path.length>currentOwner.parent.path.length) {
          owners.set(path,{parent,candidate,project:projectsByPath.get(path),path});
        }
      }
    }
    const childrenByParent=new Map();
    for (const child of owners.values()) {
      const {id}=child.parent;
      if (!childrenByParent.has(id)) childrenByParent.set(id,[]);
      childrenByParent.get(id).push(child);
    }
    return this.projects.filter(project=>!owners.has(project.path)).map(project=>({
      project,
      children:(childrenByParent.get(project.id)||[]).sort((a,b)=>a.candidate.relativePath.localeCompare(b.candidate.relativePath)),
      discovered:this.sessions[project.id]?.discovery?.result?.repositories.length
    }));
  },
  get cloud(){return Object.values(this.s.remotes).flatMap(r=>r.refs)},
  get remoteChecks(){return (this.s.repo?.remotes||[]).map(name=>this.s.remotes[name]||{name,refs:[],pending:true});},
  get cloudCountText(){
    const checks=this.remoteChecks;
    if (!checks.length) return '未配置远端';
    const verified=checks.filter(r=>r.timestamp&&!r.pending&&!r.error).length;
    if (verified===checks.length) return String(this.cloud.length);
    if (checks.some(r=>r.pending)) {
      if (this.s.lastCloudCount!=null) return this.s.lastCloudCount+' · 更新中';
      return verified ? '已核对 '+verified+'/'+checks.length+' 个远端 · 查询中' : '查询中';
    }
    if (verified) return '已核对 '+verified+'/'+checks.length+' 个远端';
    return checks.every(r=>r.timestamp) ? '上次核对 '+this.cloud.length+' 个 · 更新失败' : '未能核对';
  },
  get refs(){return [
    ...(this.s.repo?.local||[]).map(r=>({...r,kind:'local'})),
    ...this.cloud.map(r=>({...r,kind:'remote'}))
  ]},
  get pairs(){return branchPairs(this.s)},
  get current(){return this.pairs.find(p=>p.local?.name===this.s.repo?.branch)?.local},
  get stale(){return (this.s.repo?.tracking||[]).filter(ref=>
    Object.values(this.s.remotes).some(r=>r.timestamp&&!r.error&&ref.name.startsWith(r.name+'/')&&!r.refs.some(x=>x.name===ref.name)))},
  get changeGroups(){return groups.map(group=>({...group,files:(this.s.repo?.files||[]).filter(f=>f.group===group.id)}))},
  get fileCount(){return new Set((this.s.repo?.files||[]).map(f=>f.path)).size},
  get headCommit(){return this.s.all.find(c=>c.sha===this.s.repo?.head)},
  get detailView(){return this.s.detailTrail.at(-1);},
  get detailOpen(){return this.detailView?.view===this.s.view;},
  get viewedBranch(){
    const branch=this.detailView?.branch;
    if (!branch) return null;
    return (branch.cloud ? this.cloud : this.s.repo?.local||[]).find(ref=>ref.name===branch.name)||null;
  },
  get branchCommit(){return this.s.all.find(c=>c.sha===this.viewedBranch?.oid);},
  get workingBranchText(){
    const name=this.s.repo?.branch;
    const branch=this.detailView?.branch;
    if (name) return !branch?.cloud&&branch?.name===name ? '当前工作分支' : '当前工作分支 '+name;
    const head=this.s.repo?.head;
    return head ? '当前工作位置 · 分离 HEAD '+this.short(head) : '当前仓库尚无提交';
  },
  get historyTip(){
    if (this.s.scope==='all') return null;
    const tip=this.refs.find(r=>r.kind+'/'+r.name===this.s.scope);
    return tip?.available===false ? null : tip?.oid;
  },
  get history(){
    if (!this.active) return [];
    if (this.s.scope==='all') return this.s.all;
    const tip=this.historyTip;
    if (!tip) return [];
    const byId = new Map(this.s.all.map(c=>[c.sha,c]));
    const seen = new Set(), pending = [tip];
    while(pending.length){
      const oid = pending.pop();
      if (seen.has(oid)) continue;
      seen.add(oid);
      for (const parent of byId.get(oid)?.parents.split(' ')||[]) if(parent) pending.push(parent);
    }
    return this.s.all.filter(c=>seen.has(c.sha));
  },
  get pageCount(){return Math.ceil(this.history.length/PAGE_SIZE)},
  get pageRange(){return (this.s.page*PAGE_SIZE+1)+'–'+Math.min((this.s.page+1)*PAGE_SIZE,this.history.length)+' / '+this.history.length},
  get matches(){
    if (!this.palette) return [];
    const q=this.query.trim().toLowerCase();
    const hits=[];
    function include(hit){
      if ((hit.kind+' '+hit.label).toLowerCase().includes(q) || hit.commit?.sha.startsWith(q)) hits.push(hit);
      return hits.length===12;
    }
    for (const project of this.projects) {
      if (include({key:'project/'+project.id,label:project.name,kind:'项目',project:project.id})) return hits;
    }
    if (this.searchScope==='all') return hits.concat(this.crossHits).slice(0,12);
    for (const ref of this.refs) {
      if (include({key:ref.kind+'/'+ref.name,label:ref.name,kind:ref.kind==='local'?'本地':'云端',ref})) return hits;
    }
    for (const commit of this.s.all) {
      if (include({key:commit.sha,label:commit.subject,kind:this.short(commit.sha),commit})) break;
    }
    return hits;
  },
  get selectedSearchHit(){
    const hits=this.matches;
    return hits.find(hit=>hit.key===this.selectedSearchKey)??hits[0];
  },
  async init(){
    this.$watch(()=>({query:this.query,keys:this.matches.map(hit=>hit.key)}),({query,keys},previous)=>{
      const selected=this.selectedSearchKey;
      if (query===previous.query && keys.includes(selected)) return;
      const index=query===previous.query
        ? Math.min(Math.max(previous.keys.indexOf(selected),0),keys.length-1)
        : 0;
      this.selectedSearchKey=keys[index]??null;
    });
    try {
      const saved=localStorage.getItem(appearanceKey);
      if (['system','light','dark'].includes(saved)) this.appearance=saved;
    } catch { /* The system appearance also works without local storage. */ }
    this.applyAppearance();
    systemDark.addEventListener('change',()=>this.applyAppearance());
    reducedMotion.addEventListener('change',()=>updateTerminalAppearance());
    if (isTauri() && /Mac/.test(navigator.platform)) {
      const root=document.documentElement;
      root.classList.add('native-mac');
      if (!matchMedia('(prefers-reduced-transparency: reduce), (prefers-reduced-transparency: no-preference)').matches) {
        const syncMaterial=async()=>{
          try { root.toggleAttribute('data-solid-material',await invoke('prefers_solid_surfaces')); }
          catch { root.setAttribute('data-solid-material',''); }
        };
        root.setAttribute('data-solid-material','');
        void syncMaterial();
        window.addEventListener('focus',syncMaterial);
      }
    }
    mountGraph(this,Alpine,PAGE_SIZE);
    compactLayout.addEventListener('change', event => {
      const right = this.s.rightDiff;
      this.compactWorkbench = event.matches;
      if (event.matches && right && this.s.view==='status') {
        this.s.rightDiff = null;
        const detail = this.openDetail(right);
        if (detail?.pending) this.loadDiff(detail);
      }
    });
    if (!isTauri()) return;
    try {
      await listen('workspace-changed',({payload})=>{if(payload===this.projectId)this.refresh(payload,false)});
      window.addEventListener('focus',()=>{
        if (!this.projectId) return;
        this.refresh(this.projectId,false);
        const repositories=this.s.discovery?.result?.repositories;
        if (repositories) this.refreshSummaries(this.projectId,repositories);
      });
      const config=await invoke('project_list');
      this.projects=config.projects;
      void this.discoverAgents();
      const id=config.selected||this.projects[0]?.id;
      if(id)await this.selectProject(id);
    } catch(error){this.report(error);}
    finally {this.loadingProjects=false;}
  },
  setAppearance(mode){
    this.appearance=mode;
    this.applyAppearance();
    try { localStorage.setItem(appearanceKey,mode); } catch { /* Keep the choice for this window. */ }
  },
  applyAppearance(){
    document.documentElement.dataset.theme=this.appearance==='system' ? (systemDark.matches?'dark':'light') : this.appearance;
    updateTerminalAppearance();
  },
  goPane(pane){
    const editorWasVisible=this.visiblePane==='editor';
    if (editorWasVisible && pane!=='editor') this.rememberEditorScroll();
    this.pane=pane;
    if (pane!=='editor' || !compactLayout.matches) return;
    const s=this.s, view=s.view, detail=this.detailView;
    this.$nextTick(()=>{
      if (this.s!==s || this.pane!=='editor' || this.palette) return;
      if (!editorWasVisible && s.view===view && this.detailView===detail) {
        const buffer=this.editorBuffer(view);
        if (buffer) buffer.scrollTop=view==='status' ? s.statusScroll : detail?.scroll??0;
      }
      this.$refs.editor.focus({preventScroll:true});
    });
  },
  report(error){this.notice=String(error);},
  short(oid){return oid?.slice(0,7)||'';},
  trackingText(branch){
    if(!branch)return this.s.repo?.head?'':'空仓库';
    if(branch.gone)return '上游已删除';
    if(this.s.remotes[branch.remote]?.error && this.s.remotes[branch.remote]?.timestamp)return '按上次云端核对结果';
    if(!branch.merge)return '尚未关联上游';
    if(branch.counts?.every(n=>n===0))return '已同步';
    if(branch.counts) {
      const [ahead,behind]=branch.counts;
      return [ahead&&'领先 '+ahead,behind&&'落后 '+behind].filter(Boolean).join(' · ');
    }
    return '同步状态待核对';
  },
  remoteText(r){
    const last=r.timestamp?' · '+new Date(r.timestamp*1000).toLocaleString('zh-CN'):'';
    if (r.pending) return r.name+' · 正在核对云端…'+(last?' · 上次成功'+last:'');
    if (r.error) return r.name+' · '+r.error+(last?' · 上次成功'+last:'');
    return r.name+(r.history_error?' · '+r.history_error:' · 已核对')+last;
  },
  async addProject(){
    if (!isTauri()) {this.report('请在 GitControl 桌面应用中选择本机文件夹。');return;}
    try {
      const p=await invoke('project_open');
      if(!p)return;
      this.rememberProject(p);
      await this.selectProject(p.id);
    }catch(error){this.report(error);}
  },
  rememberProject(project){
    if (!this.projects.some(item=>item.id===project.id)) this.projects.push(project);
  },
  async selectProject(id,focusTerminal=true){
    this.goPane('editor');
    this.notice='';
    if(!this.sessions[id])this.sessions[id]=session();
    this.selectionVersion++;
    this.projectId=id;
    if (this.s.repo?.kind==='folder') {
      this.projectsOpen=true;
      this.s.childrenOpen=true;
    }
    this.$nextTick(()=>this.syncTerminal(focusTerminal));
    selectionQueue=selectionQueue.then(()=>invoke('project_select',{projectId:id})).catch(error=>this.report(error));
    await selectionQueue;
    if (this.projectId===id) await this.refresh(id,true);
  },
  async scanChildren(id){
    const s=this.sessions[id];
    if (s?.repo?.kind!=='folder'||s.discovery?.busy) return;
    s.childOpenError='';
    s.discovery={busy:true,result:s.discovery?.result,error:''};
    const request=s.discovery;
    try {
      const result=await invoke('project_scan_children',{parentProjectId:id});
      if (this.sessions[id]===s&&s.discovery===request) {
        s.discovery={busy:false,result,error:''};
        this.refreshSummaries(id,result.repositories);
      }
    } catch(error) {
      if (this.sessions[id]===s&&s.discovery===request) s.discovery={busy:false,result:null,error:String(error)};
    }
  },
  async openChild(parentId,relativePath,after){
    if (this.openingChild) return;
    this.openingChild=true;
    this.notice='';
    const parent=this.sessions[parentId];
    if (parent) parent.childOpenError='';
    const version=this.selectionVersion;
    try {
      const project=await invoke('project_open_child',{parentProjectId:parentId,relativePath});
      this.rememberProject(project);
      if (this.selectionVersion===version) {
        await this.selectProject(project.id);
        if (this.selectionVersion===version+1 && this.projectId===project.id && this.active) after?.();
      }
    } catch(error) {
      if (this.selectionVersion===version) {
        if (this.projectId===parentId&&parent) parent.childOpenError=String(error);
        else this.report(error);
      }
    } finally {
      this.openingChild=false;
    }
  },
  async removeProject(id){
    try{
      const settings=await invoke('project_remove',{projectId:id});
      await closeTerminal(id).catch(()=>{});
      delete this.sessions[id];
      this.projects=settings.projects;
      if(this.projectId===id){
        this.selectionVersion++;
        this.projectId=null;
        if(settings.selected)await this.selectProject(settings.selected);
      }
    }catch(error){this.report(error);}
  },
  async refresh(id,network){
    const s=this.sessions[id];
    if(!s)return;
    if (s.refreshing) {s.dirty=true; if(network)s.wantNetwork=true;return;}
    s.refreshing=true;
    let repository;
    try{
      repository=await invoke('repo_status',{projectId:id});
      if (this.sessions[id]!==s) return;
      if (repository.kind==='git' && remoteStateChanged(s.repo,repository)) network=true;
      const sameVersion=!s.repo || (s.repo.branch===repository.branch && s.repo.head===repository.head);
      s.repo=repository;
      const selected=sameVersion ? s.selected.filter(old=>repository.files.some(f=>f.group===old.group&&f.path===old.path&&f.stamp===old.stamp)) : [];
      if (!s.commitBusy && selected.length!==s.selected.length) s.commitError='部分已选改动发生变化，请查看差异后重新选择。';
      s.selected=selected;
      s.error='';
      for (const name of Object.keys(s.remotes)) if (!repository.remotes.includes(name)) delete s.remotes[name];
      if (repository.kind==='folder') {
        if (network&&this.projectId===id) {
          this.projectsOpen=true;
          s.childrenOpen=true;
        }
        if (!s.discovery) this.scanChildren(id);
        else if (network && s.discovery.result) this.refreshSummaries(id,s.discovery.result.repositories);
      } else {
        s.discovery=null;
      }
      this.loadHeadFiles(id,repository.head);
      this.updateGraph(id);
    } catch(error){s.error=String(error);s.repo=null;s.discovery=null;}
    finally{s.refreshing=false;}
    if (repository?.kind==='git'&&network) this.refreshRemotes(id);
    if(s.dirty){const again=s.wantNetwork;s.dirty=false;s.wantNetwork=false;setTimeout(()=>this.refresh(id,!!again),140);}
  },
  async refreshRemotes(id){
    const s=this.sessions[id];
    if (s.refreshingRemote) {s.remoteDirty=true;return;}
    s.refreshingRemote=true;
    try {
      do {
        s.remoteDirty=false;
        const names=s.repo.remotes;
        s.lastCloudCount=names.every(name=>s.remotes[name]?.timestamp)
          ? names.reduce((count,name)=>count+s.remotes[name].refs.length,0)
          : null;
        await Promise.allSettled(names.map(async name=>{
          s.remotes[name]={name,refs:[],...s.remotes[name],pending:true};
          const stillConfigured=()=>this.sessions[id]===s&&s.repo?.remotes.includes(name);
          const events=new Channel();
          let finished=false;
          events.onmessage=remote=>{
            if (!finished&&stillConfigured()) {
              s.remotes[name]=remote;
              this.updateGraph(id);
            }
          };
          try {
            const remote=await invoke('remote_refresh',{projectId:id,name,events});
            if (stillConfigured()) s.remotes[name]=remote;
          } catch(error){
            if (stillConfigured()) s.remotes[name]={...s.remotes[name],pending:false,error:String(error)};
          } finally {finished=true;}
        }));
        if (this.sessions[id]!==s) return;
        this.updateGraph(id);
      } while (s.remoteDirty && s.repo?.kind==='git');
    } finally {s.refreshingRemote=false;}
  },
  async updateGraph(id){
    const s=this.sessions[id], repo=s?.repo;
    if(repo?.kind!=='git')return;
    const pairs=branchPairs(s).filter(p=>p.local);
    const targets=pairs.map(({local,remote})=>{
      const upstream=local.remote==='.'?repo.local.find(b=>'refs/heads/'+b.name===local.merge)?.oid:remote?.available?remote.oid:null;
      return [local.name,local.oid,upstream];
    }).filter(row=>row[2]);
    const comparisonKey=JSON.stringify(targets);
    if(s.comparisonKey!==comparisonKey){
      s.comparisonKey=comparisonKey;
      s.counts={};
      if(targets.length){
        invoke('repo_compare',{projectId:id,pairs:targets.map(([,a,b])=>[a,b])}).then(counts=>{
          if(s.comparisonKey===comparisonKey)s.counts=Object.fromEntries(targets.map(([name],i)=>[name,counts[i]]));
        }).catch(error=>{if(this.projectId===id)this.report(error);});
      }
    }
    const tips=Array.from(new Set([
      repo.head, ...repo.local.map(ref=>ref.oid), ...repo.tags.map(tag=>tag.oid),
      ...Object.values(s.remotes).flatMap(remote=>remote.refs.filter(ref=>ref.available).map(ref=>ref.oid))
    ].filter(Boolean))).sort();
    const key=tips.join('|');
    if(s.historyKey===key)return;
    s.historyKey=key;
    s.graphBusy=true;
    try {
      const history=await invoke('repo_history',{projectId:id,tips});
      if(s.historyKey!==key)return;
      s.all=history;
      s.revision++;
      if(this.projectId===id){
        if(s.focusSha&&history.some(c=>c.sha===s.focusSha))this.locate(s.focusSha);
        else s.page=Math.min(s.page,Math.max(0,this.pageCount-1));
      }
    } catch(error){if(s.historyKey===key){s.historyKey=null;if(this.projectId===id)this.report(error);}}
    finally{if(s.historyKey===key||s.historyKey===null)s.graphBusy=false;}
  },
  async loadHeadFiles(id,oid){
    const s=this.sessions[id];
    if (s.headFilesOid===oid&&!s.headFilesError) return;
    s.headFilesOid=oid;
    s.headFiles=null;
    s.headFilesError=false;
    s.headFilesOpen=false;
    if (!oid) return;
    const request=crypto.randomUUID();
    s.headFilesRequest=request;
    try{
      const files=await invoke('repo_detail',{projectId:id,oid});
      if (s.headFilesOid===oid&&s.headFilesRequest===request) s.headFiles=files;
    }catch{
      if (s.headFilesOid===oid&&s.headFilesRequest===request) s.headFilesError=true;
    }
  },
  isSelected(file){return this.s.selected.some(f=>f.path===file.path&&f.group===file.group);},
  toggleFile(file){if(this.s.commitBusy||file.group==='conflict')return;this.s.selected=this.isSelected(file)?this.s.selected.filter(f=>f.path!==file.path||f.group!==file.group):this.s.selected.concat(file);},
  scopeChanged(){this.s.page=0;this.s.focusSha=null;},
  turnPage(amount){this.s.page=Math.max(0,Math.min(this.pageCount-1,this.s.page+amount));this.s.focusSha=null;},
  locate(oid,reveal=false){
    let index=this.history.findIndex(c=>c.sha===oid);
    if(index<0&&this.s.scope!=='all'){this.s.scope='all';index=this.history.findIndex(c=>c.sha===oid);}
    if(index<0){this.report('此提交暂未载入，请刷新后再定位。');return;}
    this.s.page=Math.floor(index/PAGE_SIZE);
    this.s.focusSha=oid;
    this.s.focusSerial++;
    if (reveal) {
      this.goPane('history');
      const s=this.s;
      this.$nextTick(()=>{
        if (this.s===s && this.pane==='history' && s.focusSha===oid) this.focusHistory(oid);
      });
    }
  },
  focusHistory(sha){
    const row=sha ? this.$refs.historyList.querySelector('[data-sha="'+CSS.escape(sha)+'"]') : null;
    row?.scrollIntoView({block:'nearest'});
    const scope=this.$refs.historyScope;
    (row||(scope.getClientRects().length ? scope : this.$refs.historyTab)).focus({preventScroll:true});
  },
  paneOrigin(pane,anchor){
    const s=this.s;
    if (pane==='history') {
      return {pane,scope:s.scope,page:s.page,sha:anchor||s.focusSha,scroll:this.$refs.historyList.scrollTop,revision:s.revision};
    }
    if (pane==='sidebar') return {pane,branchKey:anchor};
    return {pane:'editor',scope:s.scope,page:s.page,sha:s.focusSha,revision:s.revision};
  },
  restorePane(origin){
    const s=this.s;
    const {pane}=origin;
    if (pane==='sidebar') this.sidebar=true;
    this.goPane(pane);
    let sha=null;
    if (origin.scope!=null) {
      s.scope=origin.scope==='all'||this.refs.some(ref=>ref.kind+'/'+ref.name===origin.scope)
        ? origin.scope : 'all';
      const index=origin.sha ? this.history.findIndex(commit=>commit.sha===origin.sha) : -1;
      s.page=index>=0 ? Math.floor(index/PAGE_SIZE) : Math.min(origin.page,Math.max(0,this.pageCount-1));
      s.focusSha=sha=index>=0 ? origin.sha : null;
    }
    this.$nextTick(()=>{
      if (this.s!==s || this.pane!==pane || this.palette) return;
      if (pane==='history') {
        if (origin.revision===s.revision && s.scope===origin.scope && s.page===origin.page) {
          this.$refs.historyList.scrollTop=origin.scroll;
        }
        this.focusHistory(sha);
      } else if (pane==='sidebar') {
        const sidebar=this.$refs.projectSidebar;
        const branch=origin.branchKey
          ? sidebar.querySelector('[data-branch-key="'+CSS.escape(origin.branchKey)+'"]') : null;
        const project=sidebar.querySelector('.project.tree[aria-current="page"]');
        const target=[branch,project,this.$refs.addProject].find(control=>control?.getClientRects().length);
        target.scrollIntoView({block:'nearest'});
        target.focus({preventScroll:true});
      } else {
        this.$refs.editor.focus({preventScroll:true});
      }
    });
  },
  editorBuffer(view){
    return this.$refs.editor.querySelector('[data-view="'+view+'"]');
  },
  rememberEditorScroll(){
    const s = this.s;
    const buffer = this.editorBuffer(s.view);
    if (!buffer?.getClientRects().length) return;
    if (s.view === 'status') s.statusScroll = buffer.scrollTop;
    else if (this.detailOpen) this.detailView.scroll = buffer.scrollTop;
  },
  showEditorView(view, scroll, direction=1){
    const s = this.s;
    const detail=this.detailView;
    s.view = view;
    this.$nextTick(() => {
      if (this.s!==s || s.view!==view || this.detailView!==detail || this.visiblePane!=='editor') return;
      const buffer = this.editorBuffer(view);
      if (!buffer) return;
      buffer.scrollTop = scroll;
      viewEntrance?.cancel();
      if (!reducedMotion.matches) viewEntrance=buffer.animate([
        {opacity:.5,transform:'translateX('+direction*4+'px)'},
        {opacity:1,transform:'translateX(0)'}
      ],{duration:150,easing:'cubic-bezier(.2,.78,.25,1)'});
    });
  },
  showStatus(){
    if (this.s.view === 'status') return;
    this.rememberEditorScroll();
    this.showEditorView('status', this.s.statusScroll,-1);
  },
  resumeDetail(){
    if (!this.detailView || this.detailOpen) return;
    this.rememberEditorScroll();
    this.showEditorView(this.detailView.view, this.detailView.scroll);
  },
  openDetail(detail,origin){
    const s = this.s;
    const previousView=s.view;
    if (this.visiblePane==='editor') this.rememberEditorScroll();
    origin??=this.paneOrigin(this.visiblePane);
    this.goPane('editor');
    if (this.detailOpen && this.detailView.key === detail.key) {
      if (origin.pane!=='editor') this.detailView.origin=origin;
      return null;
    }
    if (s.view === 'status') s.detailTrail = [];
    s.detailTrail.push({ ...detail, scroll: 0, origin, previousView });
    this.showEditorView(detail.view, 0);
    return this.detailView;
  },
  backFromDetail(){
    const s=this.s;
    const closed=s.detailTrail.pop();
    if (!closed) return;
    const detail = this.detailView;
    const view=closed.previousView==='chat' || closed.previousView===detail?.view
      ? closed.previousView : 'status';
    this.showEditorView(view,view==='status' ? s.statusScroll : detail?.scroll??0,-1);
    if (closed.origin.pane!=='editor') {
      this.restorePane(closed.origin);
    } else {
      const oid = detail?.commit?.sha ?? detail?.branch?.oid;
      if (this.detailOpen && oid && s.all.some(commit=>commit.sha===oid)) this.locate(oid);
      this.$refs.editor.focus({preventScroll:true});
    }
  },
  closeDetail(){
    this.s.detailTrail = [];
    this.goPane('editor');
    this.showEditorView('status', this.s.statusScroll,-1);
    this.$refs.editor.focus({preventScroll:true});
  },
  async inspect(commit,origin){
    if (!commit) return;
    const id = this.projectId;
    const detail = this.openDetail({view:'commit', key:'commit/'+commit.sha, title:this.short(commit.sha), commit, files:null, error:''},origin);
    if (this.s.all.some(item=>item.sha===commit.sha)) this.locate(commit.sha);
    if (!detail) return;
    try {
      detail.files = await invoke('repo_detail',{projectId:id,oid:commit.sha});
      detail.error = '';
    } catch(error) {
      detail.error = String(error);
    }
  },
  branchView(branch,cloud,origin){
    const key = (cloud ? 'remote/' : 'local/') + branch.name;
    this.openDetail({view:'branch', key, title:branch.name, branch:{...branch,cloud}},origin);
    if (branch.available !== false) this.locate(branch.oid);
  },
  async copyBranchOid(){
    const oid=this.viewedBranch?.oid;
    if (!oid) return;
    try {await navigator.clipboard.writeText(oid);this.notice='已复制完整提交号。';}
    catch(error){this.report(error);}
  },
  async viewPatch(file,oid){
    const key = JSON.stringify([oid, file.group, file.path, file.stamp]);
    const label = oid ? this.short(oid) : groups.find(group => group.id === file.group)?.label;
    const next = {view:'diff', key, title:file.path, label, file:{...file}, oid:oid??null,
      head:this.s.repo?.head??null, pending:true, patch:null, error:'', raw:false, lookup:null};
    if (!oid && !this.compactWorkbench && this.s.view==='status') {
      this.s.rightDiff=next;
      await this.loadDiff(this.s.rightDiff);
      return;
    }
    const detail=this.openDetail(next);
    if (detail) await this.loadDiff(detail);
  },
  openSearch(trigger){
    if (this.$refs.agentLoginDialog.open) return;
    searchExit?.effect?.target?.remove();
    searchExit?.cancel();
    searchExit=null;
    if (!this.palette) {
      searchTrigger=trigger||document.activeElement;
      this.query='';
      this.searchScope='project';
      this.crossHits=[];
      this.crossBusy=false;
      this.crossFailures=0;
      this.selectedSearchKey=null;
      this.palette=true;
      const dialog=this.$refs.searchDialog;
      dialog.showModal();
    }
    this.$refs.search.focus();
  },
  moveSearch(amount){
    const hits=this.matches;
    if (!hits.length) return;
    const current=Math.max(0,hits.findIndex(hit=>hit.key===this.selectedSearchKey));
    const next=hits[(current+amount+hits.length)%hits.length];
    this.selectedSearchKey=next.key;
    document.getElementById(this.searchOptionId(next.key))?.scrollIntoView({block:'nearest'});
  },
  searchOptionId(key){return 'search-hit-'+encodeURIComponent(key);},
  searchKey(event){
    if (event.isComposing || event.keyCode===229) return;
    if (!['ArrowDown','ArrowUp','Enter'].includes(event.key)) return;
    event.preventDefault();
    if (event.key==='Enter') this.choose(this.selectedSearchHit?.key);
    else this.moveSearch(event.key==='ArrowDown'?1:-1);
  },
  closeSearch(restore=true){
    if (!this.palette) return;
    const dialog=this.$refs.searchDialog;
    const bounds=restore && !reducedMotion.matches ? dialog.firstElementChild.getBoundingClientRect() : null;
    const trigger=searchTrigger;
    searchTrigger=null;
    this.palette=false;
    dialog.close();
    if (restore) {
      const target=trigger?.getClientRects().length&&trigger!==document.body&&!trigger.disabled ? trigger : this.$refs.launch;
      target.focus({preventScroll:true});
    }
    if (!bounds || this.palette) return;
    const sheet=document.createElement('div');
    sheet.className='picker glass search-exit';
    sheet.setAttribute('aria-hidden','true');
    sheet.inert=true;
    Object.assign(sheet.style,{
      top:bounds.top+'px',left:bounds.left+'px',width:bounds.width+'px',height:bounds.height+'px'
    });
    dialog.after(sheet);
    const exit=sheet.animate([{opacity:1},{opacity:0,transform:'translateY(-4px)'}],{duration:120,easing:'ease-in'});
    searchExit=exit;
    const cleanup=()=>{
      sheet.remove();
      if (searchExit===exit) searchExit=null;
    };
    exit.finished.then(cleanup,cleanup);
  },
  choose(key){
    const hit=key&&this.matches.find(item=>item.key===key);
    if (!hit) return;
    const trigger=searchTrigger;
    let pane=this.visiblePane;
    if (!compactLayout.matches) {
      if (trigger?.closest('.history')) pane='history';
      else if (trigger?.closest('#project-sidebar')) pane='sidebar';
    }
    const anchor=pane==='history' ? trigger?.closest('.graph-row')?.dataset.sha : trigger?.dataset.branchKey;
    const origin=this.paneOrigin(pane,anchor);
    this.closeSearch(false);
    if(hit.repository){this.openCrossHit(hit);return;}
    if(hit.project)this.selectProject(hit.project,false);
    if(hit.commit)this.inspect(hit.commit,origin);
    if(hit.ref)this.branchView(hit.ref,hit.ref.kind==='remote',origin);
    this.$refs.editor.focus({preventScroll:true});
  },
  shortcut(event){
    if(event.isComposing||event.keyCode===229)return;
    if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='p'){
      event.preventDefault();
      event.stopPropagation();
      this.openSearch();
    }
    if(event.key==='Escape'&&this.palette){
      event.preventDefault();
      event.stopPropagation();
      this.closeSearch();
    }
  },
  openChat(){
    if (!this.project) return;
    const s=this.s;
    const pane=this.visiblePane;
    if (pane==='editor' && s.view==='chat') return;
    if (pane==='editor') this.rememberEditorScroll();
    const view=s.view==='chat' ? s.chatOrigin?.view??'status' : s.view;
    s.chatOrigin={...this.paneOrigin(pane), view};
    s.chatCommit=pane==='editor' && s.view==='commit' && this.detailOpen
      ? this.detailView.commit.sha : null;
    this.goPane('editor');
    s.chat=true;
    s.view='chat';
    void this.discoverAgents();
  },
  toggleChat(){
    const s=this.s;
    if (this.visiblePane!=='editor' || s.view!=='chat') {this.openChat();return;}
    const origin=s.chatOrigin;
    s.chat=false;
    s.chatCommit=null;
    s.chatOrigin=null;
    const view=origin?.view||'status';
    this.showEditorView(view,view==='status' ? s.statusScroll : this.detailView?.scroll??0,-1);
    if (origin) this.restorePane(origin);
  },
  toggleTerminal(){
    if(!this.project)return;
    const offscreen=compactLayout.matches&&this.pane!=='editor';
    this.goPane('editor');
    this.s.term=offscreen||!this.s.term;
    this.$nextTick(()=>this.syncTerminal());
  },
  openProjectTerminal(){this.s.term=true;this.$nextTick(()=>this.syncTerminal());},
  syncTerminal(stealFocus=true){
    const id=this.projectId;
    showTerminal(id,this.s.term,this.$refs.terminals,error=>this.report(error),
      ()=>stealFocus&&this.projectId===id&&this.s.term&&!this.palette&&(!compactLayout.matches||this.pane==='editor'));
  },
  async closeTerminal(){this.s.term=false;try{await closeTerminal(this.projectId);}catch(error){this.report(error);}},
  async discoverAgents(){
    if (!isTauri() || this.scanningAgents || this.switchingAgent || Date.now()-this.lastAgentScan<10000) return;
    this.scanningAgents=true;
    try {
      const {options, selected}=await invoke('agent_list');
      this.agentOptions=options;
      this.selectedAgent=selected||'';
      this.lastAgentScan=Date.now();
      this.$nextTick(()=>{this.$refs.agentChoice.value=this.selectedAgent;});
    } catch(error){this.report(error);}
    finally {this.scanningAgents=false;}
  },
  async chooseAgent(event){
    const choice=event.target;
    const kind=choice.value;
    if (!kind || kind===this.selectedAgent) return;
    this.switchingAgent=true;
    try {
      await invoke('agent_select',{kind});
      this.selectedAgent=kind;
    } catch(error){
      choice.value=this.selectedAgent;
      this.report(error);
    }
    finally {this.switchingAgent=false;}
  },
  async openAgentLogin(message){
    const dialog=this.$refs.agentLoginDialog;
    if (dialog.open) return;
    const session=this.s;
    this.agentTerminalMode=false;
    this.agentLoginError='';
    dialog.showModal();
    await showTerminal(agentLoginTerminal,true,this.$refs.agentLoginTerminal,
      error=>{this.agentLoginError=String(error);},()=>dialog.open,{
        command:'agent_login_open',
        onExit:exit=>{
          if (exit !== 0) {
            this.agentLoginError='登录未完成。可以关闭窗口后再试一次。';
            return;
          }
          message.authRequired=false;
          message.text='Antigravity 已登录。可以重新发送问题。';
          if (!session.draft) session.draft=message.question;
          void this.closeAgentLogin();
        }
      });
  },
  async openAgentTerminal(){
    const dialog=this.$refs.agentLoginDialog;
    if (dialog.open || this.selectedAgentOption?.questions !== false) return;
    this.agentTerminalMode=true;
    this.agentLoginError='';
    dialog.showModal();
    await showTerminal(agentLoginTerminal,true,this.$refs.agentLoginTerminal,
      error=>{this.agentLoginError=String(error);},()=>dialog.open,{
        command:'agent_terminal_open',
        args:{projectId:this.projectId}
      });
  },
  async closeAgentLogin(){
    this.$refs.agentLoginDialog.close();
    try {await closeTerminal(agentLoginTerminal);}
    catch(error){this.report(error);}
    if (this.agentTerminalMode) this.$refs.agentChoice.focus({preventScroll:true});
    else this.$refs.agentInput.focus({preventScroll:true});
  },
  scrollChat(){this.$nextTick(()=>{const area=this.$refs.messages;if(area)area.scrollTop=area.scrollHeight;});},
  async ask(question,purpose=null){
    const text=question.trim(), id=this.projectId, s=this.s;
    if(!text||!id||s.running||this.switchingAgent)return;
    const requestId=crypto.randomUUID(), events=new Channel();
    s.draft='';s.running=requestId;
    s.messages.push({id:requestId+':user',who:'你',text},{id:requestId+':assistant',who:'Agent',text:'',markdown:'',question:text});
    const answer=s.messages.at(-1);
    events.onmessage=event=>{
      if(event.kind==='started')answer.who=event.agent;
      if(event.kind==='delta')answer.markdown+=event.text;
      if(event.kind==='auth_required')answer.authRequired=true;
      if(event.kind==='commit'){
        answer.who='Git';
        answer.text='已提交 '+this.short(event.oid)+' · '+event.message;
        this.refresh(id,false);
      }
      if(this.projectId===id)this.scrollChat();
    };
    try {await invoke('agent_send',{projectId:id,requestId,question:text,selected:s.selected,commitOid:s.chatCommit,purpose,events});}
    catch(error){answer.text=String(error);}
    finally{s.running=null;if(this.projectId===id)this.scrollChat();}
  },
  async cancel(){if(this.s.running)try{await invoke('agent_cancel',{projectId:this.projectId,requestId:this.s.running});}catch(error){this.report(error);}}
}, Object.getOwnPropertyDescriptors(workbench(compactLayout))));

const app = document.querySelector('#app');
app.innerHTML = template
  .replace('<!-- editor-diff -->', diffTemplate.replaceAll('DIFF_DETAIL','detailView'))
  .replace('<!-- side-diff -->', diffTemplate.replaceAll('DIFF_DETAIL','rightPreview'));
createIcons({
  root: app,
  inTemplates: true,
  attrs: {'aria-hidden':'true','focusable':'false'},
  icons: {
    ArrowDownToLine, ArrowLeft, ArrowUp, ArrowUpFromLine, ArrowUpRight, ChevronLeft, ChevronRight,
    ChevronUp, CircleCheck, Cloud, CornerDownLeft, FileDiff, Files, FileText, FolderGit2, FolderOpen,
    GitBranch, GitCommitHorizontal, GitCompareArrows, GitGraph, GitPullRequest, History, Layers, LayoutDashboard,
    ListFilter, MessageSquare, Monitor, Moon, PanelLeft, Plus, RefreshCw, Search,
    Square, SquareTerminal, Sun, Tag, X
  }
});
app.setAttribute('@keydown.window.capture','shortcut($event)');
Alpine.start();
