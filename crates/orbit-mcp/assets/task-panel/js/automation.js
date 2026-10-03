/** Workspace Automation. Refreshes are observational; only explicit actions write. */
window.OrbitAutomation = ({el, ui, tool, open, current}) => {
  let scope='routines', offset=0, next=null, revision=0, fresh=false, busy=false, selected=null, items=[], confirmation=null;
  const uncertain=new Map();
  const scopes={routines:['Routines','routine'],auto_tasks:['Auto-tasks','auto_task'],jobs:['Jobs','job']};
  const feedback=text=>{el('automation-feedback').textContent=text;};
  const key=()=>`${current()}|${scope}`;
  const schedule=value=>value?.every_minutes?`Every ${value.every_minutes} minutes`:value?.deliveries_landed?'After verified deliveries':value?.state?'When state matches':value?.cron?`Cron · ${value.cron}`:'On demand';
  function button(text,run){const b=ui.node('button',text);b.type='button';b.disabled=!fresh||busy||uncertain.has(key());b.addEventListener('click',run);return b;}
  function reset(){
    revision++;fresh=false;items=[];selected=null;confirmation=null;offset=0;next=null;
    el('automation-list').replaceChildren();el('automation-detail').replaceChildren();
    el('automation-notes').textContent='';el('automation-page').textContent='';
    el('automation-previous').disabled=el('automation-next').disabled=true;
    feedback(uncertain.get(key())||'');
  }
  function renderDetail(){
    const target=el('automation-detail');target.replaceChildren();
    if(!selected)return;
    const item=items.find(i=>i.name===selected);if(!item){selected=null;return;}
    target.append(ui.node('h3',item.name));
    if(item.description)ui.markdown(target,item.description);
    for(const [title,value] of [['Target',item.target],['Schedule',schedule(item.schedule)],['Next evaluation',scope==='jobs'?null:item.next_due&&item.state==='scheduled'?ui.time(item.next_due):item.state?ui.label(item.state):null],['Deduplication',item.dedupe?ui.label(item.dedupe):null],['Paused on this host',item.paused_at?ui.time(item.paused_at):null],['Last fire',item.last_fire],['Parallel runs',item.max_active_runs],['Steps',item.steps],['Availability',item.skip_reason||item.run_reason]]){
      if(value!=null)ui.field(target,title,value);
    }
    if(item.last_run?.run_id){
      const link=ui.node('button',`Last run · ${ui.label(item.last_run.state)} · ${ui.time(item.last_run.at)}`,'text-button');
      link.type='button';link.addEventListener('click',()=>open('run',item.last_run.run_id));target.append(link);
    }
    const actions=ui.node('div',null,'automation-actions');
    if(item.toggle_available===true)actions.append(button(item.enabled?'Disable definition':'Enable definition',()=>void act('toggle',item)));
    if(confirmation && (confirmation.name!==item.name||confirmation.signature!==JSON.stringify(item))) {
      confirmation=null;feedback('Definition changed. Review it before confirming an action.');
    }
    if(confirmation){
      const action=confirmation.action;
      const box=ui.node('div',null,'action-confirmation');
      box.append(ui.node('p',action==='mint'?'Mint one task now? This ignores the schedule, enabled flag and dedupe policy. The new task is not dispatched.':`Submit ${item.name} now using its default input? This starts or queues a workflow on the selected host.`));
      box.append(button(action==='mint'?'Mint one task':'Submit job',()=>void act(action,item)),button('Cancel',()=>{confirmation=null;renderDetail();}));
      actions.replaceChildren(box);
    }else for(const action of ['mint','run'])if(item[`${action}_available`]===true){
      actions.append(button(action==='mint'?'Mint task…':'Run job…',()=>{
        confirmation={action,name:item.name,signature:JSON.stringify(item)};renderDetail();
      }));
    }
    target.append(actions);
  }
  function render(data){
    items=data.items;
    const list=el('automation-list');list.replaceChildren();
    for(const item of items){
      const row=ui.node('button',null,'entity-row');row.type='button';
      row.setAttribute('aria-expanded',String(selected===item.name));row.setAttribute('aria-controls','automation-detail');
      row.append(ui.node('div',item.name,'entity-title'));
      const meta=ui.node('div',null,'entity-meta');meta.append(ui.badge(item.state|| (item.enabled?'enabled':'disabled')),ui.node('span',scope==='jobs'?`${item.steps} steps · ${item.max_active_runs} parallel`:schedule(item.schedule)));
      row.append(meta);if(item.target)row.append(ui.node('span',item.target,'entity-time'));
      row.addEventListener('click',()=>{selected=selected===item.name?null:item.name;confirmation=null;render(data);});list.append(row);
    }
    if(!items.length)list.append(ui.node('p',`No ${scopes[scope][0].toLowerCase()} in this workspace.`,'empty-state'));
    next=Number.isInteger(data.pagination?.next_offset)?data.pagination.next_offset:null;
    el('automation-previous').disabled=offset===0;el('automation-next').disabled=next==null;
    el('automation-page').textContent=`${items.length?offset+1:0}–${offset+items.length} of ${data.total}`;
    el('automation-notes').textContent=[data.observation,...(data.notes||[])].filter(Boolean).join('\n');
    renderDetail();
  }
  async function refresh(){
    const workspace=current(), expectedScope=scope, g=++revision;
    fresh=false;renderDetail();
    try{
      const data=await tool('orbit_desktop_read',{workspace,scope,offset,limit:25});
      if(g!==revision||workspace!==current()||expectedScope!==scope)return;
      if(data.schema_version!==1||data.workspace!==workspace||data.scope!==scope||!Array.isArray(data.items)||data.controls_authorized!==true)throw new Error('Incompatible automation projection');
      fresh=true;render(data);feedback(uncertain.get(key())||'');
      return true;
    }catch(error){
      if(g!==revision||workspace!==current())return;
      fresh=false;renderDetail();feedback(`Automation unavailable. ${error.message} Refresh or choose another workspace.`);
      return false;
    }
  }
  async function act(action,item){
    if(!fresh||busy||uncertain.has(key())||!items.includes(item))return;
    const workspace=current(), scopeAtStart=scope, g=revision, uncertainKey=key();
    const args={workspace,action,kind:scopes[scope][1],name:item.name};
    if(action==='toggle'){
      args.expected_enabled=item.enabled;args.enabled=!item.enabled;
      if(scope==='routines')args.target=item.target;
    }
    if(action==='mint')args.acknowledge_unconditional=true;
    confirmation=null;busy=true;renderDetail();feedback('Applying action…');
    try{
      const result=await tool('orbit_desktop_automation',args);
      if(result.schema_version!==1||result.workspace!==workspace||result.name!==item.name||result.action!==action||result.kind!==args.kind||(action==='toggle'&&result.enabled!==args.enabled)||(action==='mint'&&typeof result.task_id!=='string')||(action==='run'&&(typeof result.run_id!=='string'||!['queued','submitted'].includes(result.state))))throw new Error('Unrecognized action receipt');
      if(g===revision&&workspace===current()&&scopeAtStart===scope){
        const refreshedRevision=revision+1;await refresh();if(refreshedRevision!==revision||workspace!==current()||scopeAtStart!==scope)return;feedback(action==='toggle'?`${item.name} ${result.enabled?'enabled':'disabled'}.`:action==='mint'?`Created ${result.task_id}. No delivery was started.`:`${result.run_id} ${result.state}.`);
        if(result.task_id)open('task',result.task_id);
        if(result.run_id)open('run',result.run_id);
      }
    }catch(error){
      const message=`${item.name}: outcome unknown. ${error.message} Inspect the definition, Tasks or Runs before another action. Writes on this panel are disabled until it is reopened.`;
      uncertain.set(uncertainKey,message);
      if(g===revision&&workspace===current()){fresh=false;feedback(message);}
    }finally{busy=false;renderDetail();}
  }
  for(const name of Object.keys(scopes))el(`automation-${name}`).addEventListener('click',()=>{
    scope=name;reset();for(const other of Object.keys(scopes))el(`automation-${other}`).setAttribute('aria-pressed',String(other===scope));void refresh();
  });
  el('automation-previous').addEventListener('click',()=>{offset=Math.max(0,offset-25);selected=null;void refresh();});
  el('automation-next').addEventListener('click',()=>{if(next!=null){offset=next;selected=null;void refresh();}});
  el('automation-refresh').addEventListener('click',()=>void refresh());
  return {refresh,reset,stale(){fresh=false;renderDetail();}};
};
