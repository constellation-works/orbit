/* Fixture bridge, used for rendered acceptance only. No real tool calls. */
const app=document.getElementById('app');
let theme='dark',live=null,stopped=false,complete=false;
let workspace='mac/orbit';
const now=new Date().toISOString();
const definitions={
 routines:[{name:'daily-maintenance',description:'Keep the workspace healthy with a daily check.',enabled:true,effective:true,target:'job:maintenance',schedule:{cron:'0 9 * * *'},state:'scheduled',toggle_available:true},{name:'after-delivery',description:'Evaluate the backlog after verified landings.',enabled:true,target:'job:maintenance',schedule:{deliveries_landed:{count:3}},state:'waiting',toggle_available:true}],
 auto_tasks:[{name:'qa-sweep',description:'Review recent deliveries for regressions. Manual mint creates one **proposed task** without starting delivery.',enabled:true,schedule:{every_minutes:120},state:'enabled',target:'Review recent deliveries',dedupe:'skip_if_open',toggle_available:true,mint_available:true}],
 jobs:[{name:'maintenance',state:'enabled',kind:'workflow',max_active_runs:1,steps:3,run_available:true,run_reason:'Submit this job with its default input',last_run:{run_id:'jrun-fixture-2',state:'success',at:now}},{name:'workspace_auto_pipeline',state:'enabled',kind:'workflow',max_active_runs:1,steps:4,run_available:false,run_reason:'Use Auto-drain for a bounded delivery window'}],
};
const tasks=[
 {id:'ORB-201',title:'Refine the plugin Control Center',status:'in_progress',priority:'high',crew:'sol',updated_at:now,dependencies:[]},
 {id:'ORB-202',title:'Verify Markdown and review evidence safely',status:'review',priority:'high',crew:'astra',updated_at:now,dependencies:[]},
 {id:'ORB-203',title:'Wait for the current delivery to release shared files',status:'blocked',priority:'medium',crew:'sol',updated_at:now,dependencies:['ORB-201']},
 {id:'ORB-204',title:'Keep narrow layouts readable with a long task title that wraps without clipping',status:'proposed',priority:'medium',crew:'luna',updated_at:now,dependencies:[]},
 {id:'ORB-206',title:'Queue the next desktop improvement',status:'backlog',priority:'medium',crew:'sol',updated_at:now,dependencies:[]},
 {id:'ORB-205',title:'Make workspace changes preserve unsent drafts',status:'done',priority:'medium',crew:'sol',updated_at:now,dependencies:[]},
];
const description='## A calmer daily workflow\n\nUse **clear hierarchy** and readable details, aligned with the Orbit dashboard.\n\n- Keep host and workspace visible\n- Preserve `unsent drafts` during refresh\n- Make the next action easy to find\n\n### Validation\n\n| Surface | Evidence |\n| --- | --- |\n| Tasks | Filter, open, edit |\n| Auto-drain | Readiness and guarded controls |\n\n```rust\nlet safe = "content stays data";\n```\n\n[Project](https://github.com/constellation-works/orbit)\n\n<script>window.__unsafe=true</script>\n<img src=x onerror="window.__unsafe=true">\n[unsafe](javascript:alert(1))';
function detail(id){const task=tasks.find(t=>t.id===id)||tasks[0];return {schema_version:1,workspace,observed_at:now,revision:'fixture-revision-1',task:{...task,description,acceptance_criteria:['Markdown renders safely','Keyboard and narrow layouts remain usable'],execution_summary:'### Validation\n\nUI fixture loaded; guarded operations tested separately.'},actions:{edit:{enabled:true},comment:{enabled:true},review:{enabled:true},complete:{enabled:true}},comments:[{by:'reviewer',at:now,body:'The task details should be **readable**, with technical evidence available on demand.'}],comments_total:1,history:[{event:'created',at:now,by:'operator'}],history_total:1,artifacts:[],artifacts_total:0};}
function read(args){
  if(definitions[args.scope])return {schema_version:1,workspace,scope:args.scope,controls_authorized:true,items:definitions[args.scope],total:definitions[args.scope].length,pagination:{next_offset:null},notes:[],observation:'Workspace definitions only. The host clock is independent; enabled does not guarantee a running scheduler.'};
  if(args.scope==='drain')return {schema_version:1,workspace,controls_authorized:true,capacity:{active_leaf_runs:live?2:0,max_active_leaf_runs:4,free_slots:live?2:4,drain_run_id:live,admissions_stopped:stopped,ends_at:live?new Date(Date.now()+3600000).toISOString():null},tasks:[{task_id:'ORB-201',eligible:true},{task_id:'ORB-202',eligible:true},{task_id:'ORB-203',eligible:false,reason:'context_lock_conflict',blocking_task_ids:['ORB-201']}]};
  if(args.scope==='task')return detail(args.id);
  if(args.scope==='run')return {schema_version:1,workspace,observed_at:now,run:{id:args.id,title:'Deliver eligible backlog',job_id:'workspace_auto',state:stopped?'success':'running',attempt:1,created_at:now,started_at:now,duration_ms:81234,steps:[{step_id:'admit',state:'success'},{step_id:'deliver',state:'running'}]},logs:{items:[],total:0}};
  const items=args.scope==='runs'?[{id:'jrun-fixture-1',job_id:'workspace_auto',state:'running',attempt:1,duration_ms:81234,created_at:now},{id:'jrun-fixture-2',job_id:'task_pr_pipeline',state:'success',attempt:1,duration_ms:382900,created_at:now}]:tasks.filter(t=>(!args.status||args.status.split(',').includes(t.status.replaceAll('_','-')))&&(!args.priority||t.priority===args.priority)&&(!args.search||(t.id+' '+t.title).toLowerCase().includes(args.search.toLowerCase())));
  const offset=args.offset||0,limit=args.limit||50;
  return {schema_version:1,workspace,items:items.slice(offset,offset+limit),total:items.length,pagination:{offset,limit,next_offset:offset+limit<items.length?offset+limit:null,total:items.length}};
}
function automate(args){
  const receipt={schema_version:1,workspace,action:args.action,kind:args.kind,name:args.name};
  if(args.action==='toggle'){const item=Object.values(definitions).flat().find(d=>d.name===args.name);item.enabled=args.enabled;item.state=args.enabled?'enabled':'disabled';return {...receipt,enabled:args.enabled};}
  if(args.action==='mint')return {...receipt,task_id:'ORB-204'};
  return {...receipt,run_id:'jrun-fixture-2',state:'submitted'};
}
function drain(args){
  if(args.action==='start'){live='jrun-fixture-live';stopped=false;complete=args.complete;return {schema_version:1,workspace,action:'start',run_id:live,state:'submitted',completion:complete?'done':'review'};}
  stopped=true;return {schema_version:1,workspace,action:'stop',outcome:'stopped',coordinators:[{run_id:live,remaining_children:[{run_id:'child-1'},{run_id:'child-2'}]}]};
}
function tools(name,args){
 workspace=args.workspace||workspace;
 if(name==='orbit_task_list')return read({...args,scope:'tasks'});
 if(name==='orbit_task_show')return read({...args,scope:'task'});
 if(name==='orbit_workflow_run_show')return read({...args,scope:'run'});
 if(name==='orbit_workflow_run_list')return args.include_catalog?{catalog:read({...args,scope:'jobs'}),runs:read({...args,scope:'runs'})}:read({...args,scope:'runs'});
 if(name==='orbit_workflow_auto')return (args.action==='status'?read:drain)({...args,scope:'drain'});
 if(name==='orbit_routine_control')return (args.action==='list'?read:automate)({...args,scope:'routines',kind:'routine'});
 if(name==='orbit_auto_task_list')return read({...args,scope:'auto_tasks'});
 if(name==='orbit_auto_task_update'||name==='orbit_auto_task_mint')return automate({...args,kind:'auto_task',action:name==='orbit_auto_task_update'?'toggle':'mint'});
 if(name==='orbit_pipeline_invoke')return automate({...args,name:args.job_name,kind:'job',action:'run'});
 if(name==='orbit_task_add'||name==='orbit_task_update')return {mutation_applied:false,refusal:{message:'This preview does not persist task writes.'}};

 if(name==='orbit_workspace_list')return {workspaces:[{selector:'mac/orbit',machine_name:'Mac',name:'Orbit'},{selector:'linux/orbit',machine_name:'dk-server-1',name:'Orbit'},{selector:'offline/orbit',machine_name:'Offline host',name:'Orbit',reachability:'unreachable'}]};
 if(name==='orbit_search')return {items:tasks.slice(0,2),total:2,truncated:false};
 throw new Error('Unsupported fixture tool '+name);
}
window.addEventListener('message',event=>{
 if(event.source!==app.contentWindow)return;
 const m=event.data;if(!m?.id)return;
 try{
  const result=m.method==='ui/initialize'?{protocolVersion:'2026-01-26',hostCapabilities:{serverTools:{},updateModelContext:{}},hostContext:{theme}}:m.method==='tools/call'?{structuredContent:tools(m.params.name,m.params.arguments)}:{};
  app.contentWindow.postMessage({jsonrpc:'2.0',id:m.id,result},location.origin);
 }catch(error){app.contentWindow.postMessage({jsonrpc:'2.0',id:m.id,error:{message:error.message}},location.origin);}
});
for(const button of document.querySelectorAll('[data-width]'))button.onclick=()=>{app.style.width=button.dataset.width+'px';app.style.maxWidth='100%';};
document.getElementById('theme').onclick=()=>{theme=theme==='dark'?'light':'dark';app.contentWindow.postMessage({jsonrpc:'2.0',method:'ui/notifications/host-context-changed',params:{hostContext:{theme}}},location.origin);};
