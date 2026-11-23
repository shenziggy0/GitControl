import { Cloud, createElement, GitBranch } from 'lucide';

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
  const origin=ref.kind==='local'?'本地':'云端';
  badge.title=origin+' '+ref.name+' · '+ref.oid;
  badge.setAttribute('aria-label',badge.title);
  const svg=createElement(ref.kind==='local'?GitBranch:Cloud,{class:'lucide','aria-hidden':'true'});
  const label=document.createElement('span');
  label.textContent=ref.name;
  badge.append(svg,label);
  return badge;
}

export function mountGraph(state, Alpine, pageSize) {
  const list=document.getElementById('history-list');
  let refsBySha = new Map();
  const height = row => refsBySha.has(row.commit.sha) ? 56 : 36;
  const nodeOffset = 18;
  const x = lane => 10+lane*13;
  let layoutKey = '';
  let rows = [];
  let renderedPage='';
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
      const button=document.createElement('button');
      button.type='button';
      button.className='graph-row'+(refsBySha.has(commit.sha)?' with-refs':'');
      button.dataset.sha=commit.sha;
      button.style.height=h+'px';
      const gutter=Math.max(lane,...edges.map(e=>Math.max(e.from,e.to)));
      button.style.paddingLeft=(x(gutter)+21)+'px';
      button.title=commit.subject+' · '+commit.author+' · '+commit.sha;
      button.setAttribute('aria-pressed','false');
      const heading=document.createElement('span');heading.className='row-heading';
      const subject=document.createElement('span');subject.className='subject';subject.textContent=commit.subject;
      const author=document.createElement('span');author.className='author';author.textContent=commit.author;
      heading.append(subject,author);button.append(heading);
      if (refsBySha.has(commit.sha)) {
        const labels=document.createElement('span');labels.className='row-pointers';
        for (const ref of refsBySha.get(commit.sha)) labels.append(refBadge(ref));
        button.append(labels);
      } else {
        const tag=state.graphTags.get(commit.sha);
        if(tag){const label=document.createElement('span');label.className='ref';label.textContent=tag;heading.append(label)}
      }
      button.onclick=()=>state.inspect(commit);
      body.append(button);y+=h;
    }
    svg.append(...nodes);
    svg.style.width=(x(maxLane)+9)+'px';svg.style.height=y+'px';
    body.prepend(svg);
    list.replaceChildren(body);list.scrollTop=0;
  }
  Alpine.effect(()=>{
    const layout = [state.projectId,state.s.revision,state.s.scope,state.s.repo?.head].join('|');
    if (layout !== layoutKey) {
      rows = historyLayout(state.history,state.s.repo?.head);
      layoutKey = layout;
    }
    const refs = state.refs;
    const key = [layout,state.s.page,state.s.repo?.head,refs.map(r=>r.kind+':'+r.name+':'+r.oid).join(',')].join('|');
    if(key!==renderedPage){
      const previous = list.scrollTop;
      const samePage = renderedPage.startsWith([layout,state.s.page].join('|')+'|');
      refsBySha = new Map();
      for (const ref of refs) {
        if (!refsBySha.has(ref.oid)) refsBySha.set(ref.oid, []);
        refsBySha.get(ref.oid).push(ref);
      }
      draw(state.s.page);
      if (samePage) list.scrollTop = previous;
      renderedPage = key;
    }
    const sha=state.s.focusSha;
    void state.s.focusSerial;
    for(const b of list.querySelectorAll('button')) b.setAttribute('aria-pressed',String(b.dataset.sha===sha));
    const node=sha&&list.querySelector('[data-sha="'+CSS.escape(sha)+'"]');
    if(node)node.scrollIntoView({block:'nearest',inline:'nearest'});
  });
}
