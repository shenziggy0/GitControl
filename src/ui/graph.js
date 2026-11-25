import { Cloud, createElement, GitBranch, Tag } from 'lucide';

// Each lane names the next ancestor reached by a line. An unmerged tip
// starts a new lane; it has no incoming edge from the preceding commit.
function historyLayout(commits, head) {
  let lanes = [];
  const trunk = new Set();
  const bySha = new Map(commits.map(c => [c.sha,c]));
  for (let c=bySha.get(head)||commits[0];c;c=bySha.get(c.parents.split(' ')[0])) trunk.add(c.sha);
  return commits.map(commit => {
    if (!lanes.includes(commit.sha)) lanes.push(commit.sha);
    const lane = lanes.indexOf(commit.sha);
    const parents = commit.parents.split(' ').filter(Boolean);
    const next = lanes.slice();
    next.splice(lane,1);
    let insertion = lane;
    for (const sha of parents) {
      if (!next.includes(sha)) next.splice(insertion++,0,sha);
    }
    const edges = [];
    lanes.forEach((sha,from) => {
      const targets = from===lane ? parents : [sha];
      for (const target of targets) {
        edges.push({from,to:next.indexOf(target),main:trunk.has(target)&&(from!==lane||trunk.has(commit.sha))});
      }
    });
    lanes=next;
    return {commit,lane,edges,main:trunk.has(commit.sha)};
  });
}

function graphElement(name, attributes) {
  const el=document.createElementNS('http://www.w3.org/2000/svg',name);
  for (const [key,value] of Object.entries(attributes)) el.setAttribute(key,value);
  return el;
}

function refBadge(ref) {
  const badge=document.createElement('span');
  badge.className='pointer '+ref.kind;
  badge.dataset.name=ref.name;
  const [origin,icon]={local:['本地',GitBranch],remote:['云端',Cloud],tag:['标签',Tag]}[ref.kind];
  badge.title=origin+' '+ref.name+' · '+ref.oid;
  badge.setAttribute('aria-label',badge.title);
  const svg=createElement(icon,{class:'lucide','aria-hidden':'true'});
  const label=document.createElement('span');
  label.textContent=ref.name;
  badge.append(svg,label);
  return badge;
}

export function mountGraph(state, Alpine, pageSize) {
  const list=document.getElementById('history-list');
  const refDialog=document.getElementById('graph-refs');
  const refList=refDialog.querySelector('ul');
  const inlineRefs=2;
  let refsBySha = new Map();
  const height = row => refsBySha.has(row.commit.sha) ? 82 : 58;
  const nodeOffset = 20;
  const x = lane => 11+lane*12;
  let layoutKey = '';
  let rows = [];
  let renderedPage='';
  let refTrigger;
  refDialog.addEventListener('close',()=>{
    refTrigger?.focus({preventScroll:true});
    refTrigger=null;
  });
  function showRefs(refs, trigger) {
    const items=document.createDocumentFragment();
    for (const ref of refs) {
      const item=document.createElement('li');
      item.append(refBadge(ref));
      items.append(item);
    }
    refList.replaceChildren(items);
    refTrigger=trigger;
    refDialog.showModal();
    refList.scrollTop=0;
  }
  function draw(page) {
    const start=page*pageSize;
    const visible=rows.slice(start,start+pageSize);
    const body=document.createElement('div');
    body.className='graph-body';
    const svg=graphElement('svg',{'aria-hidden':'true',class:'lane-canvas'});
    let y=0, maxLane=0;
    function edge(e,top,bottom) {
      maxLane=Math.max(maxLane,e.from,e.to);
      const mid=(top+bottom)/2;
      const d='M '+x(e.from)+' '+top+' C '+x(e.from)+' '+mid+' '+x(e.to)+' '+mid+' '+x(e.to)+' '+bottom;
      svg.append(graphElement('path',{d,class:e.main?'trunk':'side-edge'}));
    }
    if (start>0) for (const e of rows[start-1].edges) edge(e,nodeOffset-height(rows[start-1]),nodeOffset);
    const nodes=[];
    for (const row of visible) {
      const {commit,lane,edges}=row;
      const h=height(row), at=y+nodeOffset;
      for (const e of edges) edge(e,at,at+h);
      maxLane=Math.max(maxLane,lane);
      const isHead=commit.sha===state.s.repo?.head;
      const merge=commit.parents.includes(' ');
      const style=isHead?'head-node':merge?'merge-node':row.main?'node':'side-node';
      nodes.push(graphElement('circle',{cx:x(lane),cy:at,r:isHead?5:merge?4.5:3.5,class:style}));
      if (merge) nodes.push(graphElement('circle',{cx:x(lane),cy:at,r:1.6,class:'merge-core'}));
      const item=document.createElement('div');
      item.className='graph-row-item';
      const button=document.createElement('button');
      button.type='button';
      button.className='graph-row';
      button.dataset.sha=commit.sha;
      button.style.height=h+'px';
      const gutter=Math.max(lane,...edges.map(e=>Math.max(e.from,e.to)));
      item.style.setProperty('--graph-gutter',(x(gutter)+21)+'px');
      button.title=commit.subject+' · '+commit.author+' · '+commit.sha;
      button.setAttribute('aria-pressed','false');
      const heading=document.createElement('span');heading.className='row-heading';
      const subject=document.createElement('span');subject.className='subject';subject.textContent=commit.subject;
      heading.append(subject);
      const meta=document.createElement('span');
      meta.className='row-meta';
      const hash=document.createElement('code');
      hash.textContent=commit.sha.slice(0,7);
      const author=document.createElement('span');
      author.className='author';author.textContent=commit.author;
      meta.append(hash,author);
      button.append(heading,meta);
      button.onclick=()=>state.inspect(commit,state.paneOrigin('history',commit.sha));
      item.append(button);
      const refs=refsBySha.get(commit.sha);
      if (refs) {
        const referenceButton=document.createElement('button');
        referenceButton.type='button';
        referenceButton.className='row-pointers';
        referenceButton.setAttribute('aria-label','查看 '+refs.length+' 个完整引用');
        referenceButton.setAttribute('aria-haspopup','dialog');
        referenceButton.onclick=()=>showRefs(refs,referenceButton);

        const labels=document.createElement('span');
        labels.className='ref-preview';
        labels.setAttribute('aria-hidden','true');
        for (const ref of refs.slice(0,inlineRefs)) labels.append(refBadge(ref));
        const action=document.createElement('span');
        action.className='ref-action';
        action.textContent='查看 '+refs.length+' 个引用';
        referenceButton.append(labels,action);
        item.append(referenceButton);
      }
      body.append(item);y+=h;
    }
    svg.append(...nodes);
    svg.style.width=(x(maxLane)+9)+'px';svg.style.height=y+'px';
    body.prepend(svg);
    list.replaceChildren(body);list.scrollTop=0;
  }
  Alpine.effect(()=>{
    const refs=state.refs;
    const scope=state.s.scope;
    const layout = [state.projectId,state.s.revision,scope,state.historyTip,state.s.repo?.head,state.active].join('|');
    if (layout !== layoutKey) {
      rows = historyLayout(state.history,state.s.repo?.head);
      layoutKey = layout;
      const lastPage=Math.max(0,Math.ceil(rows.length/pageSize)-1);
      if (state.s.page>lastPage) state.s.page=lastPage;
    }
    const tags = (state.s.repo?.tags||[]).filter(tag=>tag.oid).map(tag=>({...tag,kind:'tag'}));
    const labels = [...refs,...tags];
    const key = [layout,state.s.page,JSON.stringify(labels.map(r=>[r.kind,r.name,r.oid]))].join('|');
    if(key!==renderedPage){
      const previous = list.scrollTop;
      const samePage = renderedPage.startsWith([layout,state.s.page].join('|')+'|');
      refsBySha = new Map();
      for (const ref of labels) {
        if (!refsBySha.has(ref.oid)) refsBySha.set(ref.oid, []);
        refsBySha.get(ref.oid).push(ref);
      }
      draw(state.s.page);
      if (samePage) list.scrollTop = previous;
      renderedPage = key;
    }
    const sha=state.s.focusSha;
    void state.s.focusSerial;
    for(const b of list.querySelectorAll('.graph-row')) b.setAttribute('aria-pressed',String(b.dataset.sha===sha));
    const node=sha&&list.querySelector('[data-sha="'+CSS.escape(sha)+'"]');
    if(node)node.scrollIntoView({block:'nearest',inline:'nearest'});
  });
}
