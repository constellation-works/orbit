import assert from 'node:assert/strict';
const { setWorkspace } = await import('./js/common.js');
const { renderTasks } = await import('./js/tasks.js');
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
const flush = async () => { for (let i = 0; i < 5; i++) await tick(); };
const response = payload => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
const task = workspace => ({ id:'ORB-1', title:`${workspace} task`, status:'review', artifacts:[], status_transitions:[{status:'done',required_field:'execution_summary'}] });
let tasks=[];
const reads=[];
const writes=[];
globalThis.fetch = async (path, options={}) => {
  const url=new URL(path,'http://dashboard.test');
  if (options.method==='POST') { writes.push(url); return response({}); }
  assert.equal(url.pathname,'/api/distributed/claims');
  return new Promise(resolve => reads.push(resolve));
};
const context={getTasks:()=>tasks,getSearchQuery:()=>'',getActiveStatuses:()=>new Set(['review']),statusOrder:['review','done'],fmtAbsTime:value=>value,refreshDashboard:()=>Promise.resolve()};
const descendants=node=>[node,...(node.children||[]).flatMap(descendants)];
const body=()=>document.getElementById('tasks-body');
const show=workspace=>{setWorkspace(workspace);tasks=[task(workspace)];renderTasks(tasks,context);};
const expand=()=>body().children.find(node=>node.dataset.key==='task-ORB-1').dispatch('click');
const approve=()=>descendants(body()).find(node=>node.className==='action approve');
show('A');
expand();
approve().dispatch('click');
const approvalRead=reads.at(-1);
show('B');
approvalRead(response({claims:[],capabilities:{}}));
await flush();
assert.equal(writes.length,0,'leaving A while claim admission loads must not approve a task in B');

show('A');
expand();
const button=approve();
const countBefore=reads.length;
button.dispatch('click');
button.dispatch('click');
assert.equal(reads.length,countBefore+1,'a pending admission read prevents duplicate approval attempts');
assert.equal(button.disabled,true,'approval stays disabled while claim admission is read');
reads.at(-1)(response({claims:[],capabilities:{}}));
await flush();
assert.equal(writes.length,1);
assert.equal(writes[0].searchParams.get('workspace'),'A');
// Resolve rendering-only reads so the fixture exits without pending fetch timers.
for(const resolve of reads)resolve(response({claims:[],capabilities:{}}));
await flush();
