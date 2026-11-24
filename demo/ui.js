function projectSession() {
  return { view:'status', chat:false, term:false, commit:null, branch:null, messages:[], outputs:[], cmd:'', draft:'' };
}

const commitIndex = new Map(repo.log.map(commit => [commit.sha, commit]));
const currentBranch = repo.branches.find(branch => branch.name === repo.branch);
const allRefs = [
  ...repo.branches.map(branch => ({...branch, kind:'local'})),
  ...repo.remotes.map(branch => ({...branch, kind:'remote'}))
];
const comparison = repo.branches.map(local => ({
  name:local.name, local, remote:repo.remotes.find(remote => remote.name===local.up)
}));
for (const remote of repo.remotes) {
  if (!comparison.some(row => row.remote?.name===remote.name)) {
    comparison.push({name:remote.name.slice(remote.name.indexOf('/')+1), local:null, remote});
  }
}
const pageSize = 60;
const histories = new Map([['all', repo.log]]);
function historyFor(scope) {
  if (histories.has(scope)) return histories.get(scope);
  const tip = allRefs.find(ref => ref.kind+'/'+ref.name===scope);
  const seen = new Set(), pending = [tip.sha];
  while (pending.length) {
    const sha = pending.pop();
    if (seen.has(sha)) continue;
    seen.add(sha);
    const parents = commitIndex.get(sha)?.parents;
    if (parents) pending.push(...parents.split(' '));
  }
  const history = repo.log.filter(c => seen.has(c.sha));
  histories.set(scope, history);
  return history;
}

function workspace() {
  const sessions = { Snapshoot:projectSession(), GitControl:projectSession() };
  const initial = new URLSearchParams(location.search).get('state');
  if (['chat','expanded'].includes(initial)) {
    sessions.Snapshoot.chat=true;
    sessions.Snapshoot.view='chat';
  }
  sessions.Snapshoot.term=['terminal','expanded'].includes(initial);
  return {
    repo, sessions, project:'Snapshoot', projects:Object.keys(sessions),
    allRefs, comparison, currentBranch, scope:'all', page:0, focusSha:null, focusSerial:0, remotesOpen:false,
    get history(){return historyFor(this.scope)},
    get pageCount(){return Math.ceil(this.history.length/pageSize)},
    get pageRange(){return (this.page*pageSize+1)+'–'+Math.min((this.page+1)*pageSize,this.history.length)+' / '+this.history.length},
    setScope(value){this.scope=value;this.page=0;this.focusSha=null},
    turnPage(amount){this.page=Math.max(0,Math.min(this.pageCount-1,this.page+amount));this.focusSha=null},
    locate(sha){
      let i = this.history.findIndex(c=>c.sha===sha);
      if(i<0){this.setScope('all');i=this.history.findIndex(c=>c.sha===sha)}
      if(i<0)return;
      this.page=Math.floor(i/pageSize);this.focusSha=sha;this.focusSerial++;
    },
    sidebar:true, palette:false, query:'', tagsOpen:false, branchesOpen:true, projectsOpen:true,
    get s(){return this.sessions[this.project]},
    get active(){return this.project==='Snapshoot'},
    get path(){return this.active?repo.path:repo.path.replace(/snapshoot$/,'GitControl')},
    get tags(){return repo.log.filter(c=>c.refs.includes('tag: '))},
    get matches(){
      const query=this.query.trim().toLowerCase();
      const matches=this.projects.map(name=>({label:name,kind:'项目',name}));
      for(const ref of allRefs)matches.push({label:ref.name,kind:ref.kind==='local'?'本地':'云端',ref});
      for(const c of repo.log)matches.push({label:c.subject,kind:c.sha,commit:c});
      return matches.filter(m=>(m.label+' '+m.kind).toLowerCase().includes(query)).slice(0,9);
    },
    openSearch(){this.palette=true;this.query='';this.$nextTick(()=>this.$refs.search.focus())},
    closeSearch(){this.palette=false;this.$nextTick(()=>this.$refs.launch.focus())},
    choose(hit){
      if(!hit)return;
      this.closeSearch();
      this.project=hit.name||'Snapshoot';
      if(hit.commit)this.inspect(hit.commit);
      if(hit.ref)this.branchView(hit.ref);
    },
    inspect(commit){this.s.commit=commit;this.s.view='commit';this.locate(commit.sha)},
    branchView(branch){this.s.branch=branch;this.s.view='branch';this.locate(branch.sha)},
    toggleChat(){
      this.s.chat=!this.s.chat;
      this.s.view=this.s.chat?'chat':'status';
    },
    toggleTerminal(){
      this.s.term=!this.s.term;
      if(this.s.term)this.$nextTick(()=>this.$refs.cmd.focus());
    },
    shortcut(event){
      if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='p'){
        event.preventDefault();this.openSearch();
      }
      if(event.key==='Escape'&&this.palette)this.closeSearch();
    },
    ask(question){
      const q=question.trim();
      if(!q)return;
      const facts=this.active?'当前分支为 master，工作区干净，与本机记录的 origin/master 同步。待推送提交为 0。':'此文件夹尚未初始化 Git。';
      const answer=/分支|状态|改动|待推送/.test(q)&&!/执行|请提交|帮我/.test(q)?facts:'此预览仅提供当前仓库的状态摘要。';
      this.s.messages.push({who:'你',text:q},{who:'仓库摘要',text:answer});this.s.draft='';
      this.$nextTick(()=>{this.$refs.messages.scrollTop=this.$refs.messages.scrollHeight});
    },
    run(){
      const cmd=this.s.cmd.trim();
      if(!cmd)return;
      const out=this.active&&Object.hasOwn(repo.commands,cmd)?repo.commands[cmd]:cmd==='pwd'?this.path:'可预览的命令：'+(this.active?Object.keys(repo.commands).join('、')+'、':'')+'pwd';
      this.s.outputs.push({cmd,out});this.s.cmd='';
      this.$nextTick(()=>{this.$refs.console.scrollTop=this.$refs.console.scrollHeight});
    }
  };
}
