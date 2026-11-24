// Each lane names the next ancestor reached by a line. An unmerged tip
// starts a new lane; it has no incoming edge from the preceding commit.
function historyLayout(commits) {
  let lanes = [];
  const trunk = new Set();
  const bySha = new Map(commits.map(c => [c.sha,c]));
  for (let c=commits[0];c;c=bySha.get(c.parents.split(' ')[0])) trunk.add(c.sha);
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
  badge.title=origin+' '+ref.name+' · '+ref.sha;
  badge.setAttribute('aria-label',badge.title);
  const svg=graphElement('svg',{'aria-hidden':'true',viewBox:'0 0 24 24'});
  svg.append(graphElement('use',{href:ref.kind==='local'?'#i-branch':'#i-cloud'}));
  const label=document.createElement('span');
  label.textContent=ref.name;
  badge.append(svg,label);
  return badge;
}

function mountGraph(state) {
  const list=document.getElementById('history-list');
  const refsBySha=new Map();
  for (const ref of allRefs) {
    if (!refsBySha.has(ref.sha)) refsBySha.set(ref.sha,[]);
    refsBySha.get(ref.sha).push(ref);
  }
  const height = row => refsBySha.has(row.commit.sha)?48:28;
  const x = lane => 10+lane*13;
  const layouts=new Map();
  let renderedPage='';
  function draw(scope,page) {
    if (!layouts.has(scope)) layouts.set(scope,historyLayout(historyFor(scope)));
    const rows=layouts.get(scope);
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
    if (start>0) for (const e of rows[start-1].edges) edge(e,14-height(rows[start-1]),14);
    const nodes=[];
    for (const row of visible) {
      const {commit,lane,edges}=row;
      const h=height(row), at=y+14;
      for (const e of edges) edge(e,at,at+h);
      maxLane=Math.max(maxLane,lane);
      const isHead=commit.sha===currentBranch.sha;
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
        const tag=commit.refs.split(', ').find(ref=>ref.startsWith('tag: '));
        if(tag){const label=document.createElement('span');label.className='ref';label.textContent=tag.slice(5);heading.append(label)}
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
    const key=state.scope+'|'+state.page;
    if(key!==renderedPage){draw(state.scope,state.page);renderedPage=key}
    const sha=state.focusSha;
    void state.focusSerial;
    for(const b of list.querySelectorAll('button')) b.setAttribute('aria-pressed',String(b.dataset.sha===sha));
    const node=sha&&list.querySelector('[data-sha="'+CSS.escape(sha)+'"]');
    if(node)node.scrollIntoView({block:'nearest',inline:'nearest'});
  });
}
