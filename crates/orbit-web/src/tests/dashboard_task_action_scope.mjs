import assert from 'node:assert/strict';
const {setWorkspace}=await import('./js/common.js');
const {renderTasks}=await import('./js/tasks.js');
const tick=()=>new Promise(resolve=>setTimeout(resolve,0));
const flush=async()=>{for(let i=0;i<5;i++)await tick();};
const response=(payload,status=200)=>({ok:status===200,status,json:async()=>payload,text:async()=>JSON.stringify(payload)});
const descendants=node=>[node,...(node.children||[]).flatMap(descendants)];
const body=()=>document.getElementById('tasks-body');
const button=cls=>descendants(body()).find(node=>node.className===cls);
let tasks=[];
let refreshes=0;
let failRefresh=false;
let finishWrite;
const writes=[];
globalThis.fetch=async(path,options={})=>{
 const url=new URL(path,'http://dashboard.test');
 if((options.method||'GET')==='GET')return response({claims:[],capabilities:{}});
 writes.push(url);
 return new Promise(resolve=>{finishWrite=resolve;});
};
window.confirm=()=>true;
const context={getTasks:()=>tasks,getSearchQuery:()=>'',getActiveStatuses:()=>new Set(['proposed','review','backlog']),statusOrder:['proposed','review','backlog'],fmtAbsTime:value=>value,refreshDashboard:()=>{refreshes++;return failRefresh?Promise.reject(new Error('refresh unavailable')):Promise.resolve();}};
const show=(workspace,id,status='review')=>{setWorkspace(workspace);tasks=[{id,title:`${workspace} task`,status,artifacts:[]}];renderTasks(tasks,context);};
const expand=()=>body().children.find(node=>node.dataset.key===`task-${tasks[0].id}`).dispatch('click');

show('A','ORB-quick','proposed');
button('task-quick approve').dispatch('click');
show('B','ORB-quick','proposed');
finishWrite(response({}));
await flush();
assert.equal(refreshes,0,'a late quick approval must not refresh another workspace');
assert.ok(!body().textContent.includes('approved and moved to backlog'));
assert.equal(button('task-quick approve').disabled,false,'A pending state must not disable B action');

show('A','ORB-archive');
expand();
button('action archive').dispatch('click');
show('B','ORB-archive');
finishWrite(response({}));
await flush();
assert.equal(refreshes,0,'a late lifecycle mutation must not refresh B');

show('A','ORB-comment');
expand();
button('action comment').dispatch('click');
const draft=descendants(body()).find(node=>node.id==='comment-draft-ORB-comment');
draft.value='A comment';
button('action comment').dispatch('click');
show('B','ORB-comment');
finishWrite(response({}));
await flush();
assert.equal(refreshes,0,'a late comment must not refresh B');

show('A','ORB-ship','backlog');
button('task-quick ship').dispatch('click');
assert.equal(writes.at(-1).searchParams.get('workspace'),'A');
show('B','ORB-ship','backlog');
finishWrite(response({run_id:'jrun-A',state:'submitted'}));
await flush();
assert.equal(refreshes,0,'a late ship result must not refresh B');
assert.ok(!body().textContent.includes('jrun-A'));
assert.equal(button('task-quick ship').disabled,false,'a dispatch in A does not hold the same task ID in B');
show('A','ORB-ship','backlog');
assert.equal(button('task-quick ship').disabled,true,'accepted work keeps its duplicate-dispatch guard when A is revisited');

// A successful dispatch followed by a failed list read remains a success. It
// must not release the guard and offer another ship of the same task.
show('C','ORB-accepted','backlog');
failRefresh=true;
button('task-quick ship').dispatch('click');
finishWrite(response({run_id:'jrun-accepted',state:'submitted'}));
await flush();
assert.ok(body().textContent.includes('Ship was accepted'),body().textContent);
assert.equal(button('task-quick ship').disabled,true,'refresh failure cannot re-arm accepted dispatch');
const acceptedWrites=writes.length;
button('task-quick ship').dispatch('click');
assert.equal(writes.length,acceptedWrites,'accepted dispatch cannot be replayed through the stale quick button');

show('D','ORB-detail-accepted','backlog');
expand();
button('action ship').dispatch('click');
finishWrite(response({run_id:'jrun-detail-accepted',state:'submitted'}));
await flush();
assert.ok(body().textContent.includes('Ship was accepted'),body().textContent);
const detailWrites=writes.length;
button('action ship').dispatch('click');
assert.equal(writes.length,detailWrites,'refresh failure cannot replay an accepted detail dispatch');

show('E','ORB-comment-accepted');
expand();
button('action comment').dispatch('click');
descendants(body()).find(node=>node.id==='comment-draft-ORB-comment-accepted').value='posted once';
button('action comment').dispatch('click');
finishWrite(response({}));
await flush();
assert.ok(body().textContent.includes('Comment was posted'),body().textContent);
assert.equal(button('action comment').disabled,true,'a posted comment must not be offered again after refresh failure');

show('F','ORB-archive-accepted');
expand();
button('action archive').dispatch('click');
finishWrite(response({}));
await flush();
assert.ok(body().textContent.includes('archive was accepted'),body().textContent);
assert.equal(button('action archive').disabled,true,'accepted lifecycle actions wait for a successful refresh');
