/** Auto-drain card. All operations are scoped to an opaque discovered destination. */
window.OrbitDrain = ({el,ui,tool,open,current}) => {
  let revision=0, data=null, fresh=false, busy=false;
  const uncertain=new Map();
  const message=text=>{el('drain-feedback').textContent=text;};
  function active(payload){
    const c=payload?.capacity||{};
    return !!(c.drain_run_id||c.pull_drain_run_id);
  }
  function controls(){
    const c=data?.capacity||{};
    el('drain-start').disabled=!fresh||busy||uncertain.has(current())||active(data)||!el('drain-form').checkValidity();
    el('drain-stop').disabled=!fresh||busy;
    for(const id of ['drain-duration','drain-concurrency','drain-complete'])el(id).disabled=busy||active(data)||!fresh;
    el('drain-live').textContent=!fresh?'Unavailable':c.admissions_stopped||c.pull_drain_admissions_stopped||c.drain_phase==='winding_down'?'Finishing admitted work':active(data)?'Running':'Idle';
    el('drain-live').dataset.status=!fresh?'unknown':active(data)?'running':'backlog';
  }
  function reset(){
    revision++;fresh=false;data=null;
    el('drain-summary').replaceChildren();
    el('drain-queue').replaceChildren();
    el('drain-window').replaceChildren();
    message(uncertain.get(current())||'');controls();
  }
  function render(payload){
    const c=payload.capacity||{}, tasks=Array.isArray(payload.tasks)?payload.tasks:[];
    const eligible=tasks.filter(t=>t.eligible===true).length;
    const waiting=tasks.length-eligible;
    const busyCount=c.active_leaf_runs??c.occupancy?.active_leaf_runs, limit=c.max_active_leaf_runs;
    const summary=el('drain-summary');summary.replaceChildren();
    for(const [title,value]of [['Running',busyCount??'—'],['Free slots',c.free_slots??'—'],['Eligible',eligible],['Waiting',waiting]]){
      const stat=ui.node('div',null,'drain-stat');stat.append(ui.node('span',title),ui.node('strong',value));summary.append(stat);
    }
    const capacity=ui.node('p',busyCount!=null&&limit!=null?`${busyCount} running · limit ${limit}`:'Capacity unavailable','muted');summary.append(capacity);
    const queue=el('drain-queue');queue.replaceChildren();
    for(const task of tasks.filter(t=>!t.eligible).slice(0,5)){
      const line=ui.node('div',null,'drain-waiting');
      if(task.task_id){const b=ui.node('button',task.task_id,'text-button');b.type='button';b.addEventListener('click',()=>open('task',task.task_id));line.append(b);}
      line.append(ui.node('span',ui.label(task.reason||'Waiting for readiness')));queue.append(line);
    }
    if(waiting>5)queue.append(ui.node('p',`+${waiting-5} more waiting in this page`,'muted'));
    queue.append(ui.node('p',c.candidate_pool_truncated||payload.truncated||payload.has_more?'Readiness is limited to 50 tasks; more tasks may be waiting.':`${tasks.length} tasks in the readiness sample (up to 50).`,'readiness-note'));
    const window=el('drain-window');window.replaceChildren();
    for(const id of [c.drain_run_id,c.pull_drain_run_id].filter(Boolean)){
      const b=ui.node('button',id,'text-button');b.type='button';b.addEventListener('click',()=>open('run',id));window.append(b);
    }
    const deadline=c.drain_ends_at||c.ends_at||payload.ends_at;
    if(deadline)window.append(ui.node('p',`Window ends ${ui.time(deadline)}`,'muted'));
    controls();
  }
  async function refresh(){
    const workspace=current(), g=++revision;
    fresh=false;controls();
    try{
      const payload=await tool('orbit_workflow_auto',{workspace,action:'status'});
      if(g!==revision||workspace!==current())return;
      if(payload.schema_version!==1||payload.workspace!==workspace||!payload.capacity||!Array.isArray(payload.tasks)||payload.controls_authorized!==true)throw new Error('Drain readiness is unavailable for this destination.');
      data=payload;fresh=true;render(payload);
      if(!uncertain.has(workspace)&&!busy)message('');
      return true;
    }catch(error){
      if(g!==revision||workspace!==current())return;
      fresh=false;controls();message(`Auto-drain unavailable. ${error.message} An operator connection and a server with desktop drain support are required.`);
      return false;
    }
  }
  async function control(action){
    const button=el(action==='start'?'drain-start':'drain-stop');
    if(button.disabled||!fresh||busy)return;
    const workspace=current(), g=revision;
    const args={workspace,action};
    if(action==='start'){
      if(!el('drain-form').reportValidity())return;
      args.for_seconds=Number(el('drain-duration').value);
      if(el('drain-concurrency').value)args.concurrency=Number(el('drain-concurrency').value);
      args.complete=el('drain-complete').checked;
    }
    busy=true;controls();
    const pending=action==='start'?'Starting window…':'Stopping new admissions and settling recorded work…';
    message(pending);
    try{
      const result=await tool('orbit_workflow_auto',args);
      if(result.schema_version!==1||result.workspace!==workspace||result.action!==action||(action==='start'&&(typeof result.run_id!=='string'||!['submitted','queued'].includes(result.state)))||(action==='stop'&&(!['stopped','idle','cancelled_queued'].includes(result.outcome)||!Array.isArray(result.coordinators))))throw new Error('Unrecognized drain receipt');
      uncertain.delete(workspace);
      const resultText=action==='start'?`Window ${result.run_id} ${result.state}. Completion: ${result.completion}.`:
        `${result.outcome==='idle'?'No live window.':'Admissions stopped.'} ${result.coordinators?.reduce((n,c)=>n+(c.remaining_children?.length||0),0)||0} admitted workers continue. Recorded settlements processed.`;
      if(g===revision&&workspace===current()){
        const refreshedRevision=revision+1;await refresh();if(refreshedRevision!==revision||workspace!==current())return;message(resultText+(fresh?'':' Refresh unavailable; the operation succeeded.'));
      }
    }catch(error){
      // A lost response is not a failed start. Never replay a dispatch on refresh.
      const text=`${workspace}: outcome unknown. ${error.message} Inspect Runs and refresh readiness before taking further action. Starting another window is disabled in this panel.`;
      uncertain.set(workspace,text);
      if(g===revision&&workspace===current()){fresh=false;message(text);}
    }finally{busy=false;controls();}
  }
  el('drain-form').addEventListener('submit',event=>{event.preventDefault();void control('start');});
  el('drain-stop').addEventListener('click',()=>void control('stop'));
  el('drain-concurrency').addEventListener('input',controls);
  el('drain-refresh').addEventListener('click',()=>void refresh());
  return {refresh,reset,stale(){fresh=false;controls();}};
};
