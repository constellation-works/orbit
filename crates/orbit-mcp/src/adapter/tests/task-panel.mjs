// Executes the served bundle against a bounded DOM and MCP Apps host fixture.
// Native desktop support requires the separate actual-client acceptance gate.
import assert from 'node:assert/strict';
import {
  readFileSync
}
from 'node:fs';
import vm from 'node:vm';
import {
  test
}
from 'node:test';
const source=process.env.ORBIT_PANEL_RESOURCE?readFileSync(process.env.ORBIT_PANEL_RESOURCE,'utf8').match(/<script>([\s\S]*?)<\/script>/)?.[1]:['presentation.js','drain.js','automation.js','task-panel.js'].map(file=>readFileSync(new URL('../../../assets/task-panel/js/'+file,import.meta.url),'utf8')).join('\n');
const flush=()=>new Promise(resolve=>setImmediate(resolve));
class Node {
  constructor(tag='div'){
    this.tag=tag;
    this.children=[];
    this.handlers={
    };
    this.value='';
    this.textContent='';
    this.disabled=false;
    this.hidden=false;
    this.dataset={
    };
    this.parentElement={hidden:false};
    this.style={
    };
    this.classList={
      add(){
      },remove(){
      },toggle(){
      }
    };
  }
  set textContent(value){this.text=String(value);this.children=[];}
  get textContent(){return this.text+this.children.map(n=>n.textContent||'').join('');}
  set innerHTML(_){
    throw new Error('Unsafe HTML rendering');
  }
  append(...nodes){
    this.children.push(...nodes);
  }
  replaceChildren(...nodes){
    this.children=nodes;
  }
  get options(){
    return this.children.filter(n=>n.tag==='option');
  }
  addEventListener(type,fn){
    this.handlers[type]=fn;
  }
  setAttribute(key,value){
    this[key]=value;
  }
  focus(){
    this.focused=true;
  }
  checkValidity(){return true;}
  reportValidity(){
    return true;
  }
  querySelectorAll(tag){
    return this.children.flatMap(n=>[...(n.tag===tag?[n]:[]),...n.querySelectorAll(tag)]);
  }
}
function fixture(){
  const nodes=new Map(),posted=[],timers=new Map(),events={
  };
  let receive;
  let nextTimer=0;
  const parent={
    postMessage:m=>posted.push(m)
  };
  const get=id=>{
    if(!nodes.has(id))nodes.set(id,new Node(id==='workspace'?'select':'div'));
    return nodes.get(id);
  };
  get('editor').hidden=true;
  get('status').value='';
  get('priority').value='';
  const document={
    getElementById:get,createElement:tag=>new Node(tag),hidden:false,documentElement:new Node(),addEventListener:(type,fn)=>events[type]=fn
  };
  vm.runInNewContext(source,{
    window:{
      parent,addEventListener:(type,fn)=>receive=fn,crypto:{
        randomUUID:()=>`request-${posted.length}`
      },navigator:{
        clipboard:{
          writeText:async()=>{
          }
        }
      }
    },document,setTimeout:fn=>{
      const id=++nextTimer;
      timers.set(id,fn);
      return id;
    },clearTimeout:id=>timers.delete(id),Date,Map,JSON,Error,Promise
  });
  const answer=(m,data,isError=false)=>receive({
    source:parent,data:{
      jsonrpc:'2.0',id:m.id,result:m.method==='tools/call'?{
        structuredContent:m.params?.arguments?.include_catalog&&data.scope==='jobs'?{catalog:data}:data,isError
      }:data
    }
  });
  const last=()=>posted.at(-1);
  const click=id=>get(id).handlers.click?.({
    preventDefault(){
    }
  });
  const init=async(caps={
    serverTools:{
    },updateModelContext:{
    }
  })=>{
    answer(posted[0],{
      protocolVersion:'2026-01-26',hostCapabilities:caps
    });
    await flush();
    assert.equal(last().params.name,'orbit_workspace_list');
    answer(last(),{
      workspaces:[{
        selector:'host-a/ws_shared',name:'shared',host:'A'
      },{
        selector:'host-b/ws_shared',name:'shared',host:'B'
      }]
    });
    await flush();
    answer(last(),list());
    await flush();
  };
  const list=(items=[])=>({
    schema_version:1,workspace:'host-a/ws_shared',items,total:1000,pagination:{
      offset:0,limit:50,truncated:true,next_offset:50
    }
  });
  const detail=(id='ORB-1',revision='rev1')=>({
    schema_version:1,workspace:'host-a/ws_shared',observed_at:'now',scope:'task',snapshot:{
      schema_version:1,revision,task:{
        id,title:'<script>hostile()</script>',description:'<img onerror=bad()>',acceptance_criteria:['Works'],status:'review'
      },actions:{
        edit:{
          enabled:true
        },comment:{
          enabled:true
        },review:{
          enabled:true
        },complete:{
          enabled:true
        }
      }
    },comments:{
      items:[],pagination:{
        next_offset:null
      }
    }
  });
  return {
    get,posted,last,answer,click,init,list,detail,receive,events,document,timers,parent
  };
}
test('discovers opaque destinations, bounds 1000-task page, renders text and sends context only on click',async()=>{
  const f=fixture();
  await f.init();
  assert.equal(f.get('workspace').options.length,2);
  const start=performance.now();
  f.click('refresh');
  f.answer(f.last(),f.list(Array.from({
    length:1000
  },(_,i)=>({
    id:`ORB-${i}`,title:'<script>evil()</script>'
  }))));
  await flush();
  assert.equal(f.get('list').querySelectorAll('button').length,50);
  assert.ok(performance.now()-start<1000);
  f.get('list').querySelectorAll('button')[1].handlers.click();
  assert.equal(f.last().params.arguments.workspace,'host-a/ws_shared');
  assert.equal(f.last().params.arguments.id,'ORB-1');
  f.answer(f.last(),f.detail());
  await flush();
  assert.equal(f.get('title').textContent,'<script>hostile()</script>');
  assert.ok(!f.posted.some(m=>m.method==='ui/update-model-context'));
  f.click('send');
  const reference=JSON.parse(f.last().params.content[0].text);
  assert.equal(reference.workspace,'host-a/ws_shared');
  assert.equal(reference.entity.id,'ORB-1');
  assert.equal(reference.authority,'none');
  assert.ok(!('description'in reference));
  assert.ok(JSON.stringify(reference).length<5000);
});
test('late navigation reads cannot replace selected identity; hidden stops polling',async()=>{
  const f=fixture();
  await f.init();
  f.answer(f.last(),f.list());
  f.click('runs');
  const old=f.last();
  f.click('tasks');
  const current=f.last();
  f.answer(current,f.list([{
    id:'ORB-1',title:'Good'
  }]));
  await flush();
  f.answer(old,f.list([{
    id:'wrong',title:'Late'
  }]));
  await flush();
  assert.equal(f.get('list').querySelectorAll('button')[0].dataset.entityKey,'ORB-1');
  assert.equal(f.get('list').querySelectorAll('button')[0].children[0].textContent,'Good');
  f.document.hidden=true;
  f.events.visibilitychange();
  assert.equal(f.timers.size,0);
  f.receive({
    source:{
    },data:{
      jsonrpc:'2.0',method:'ui/resource-teardown'
    }
  });
  assert.equal(f.get('refresh').disabled,false);
});
test('guarded comment carries revision; lost response retries same identity and draft',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('comment').value='My preserved comment';
  f.get('comment-form').handlers.submit({
    preventDefault(){
    }
  });
  const write=f.last();
  assert.equal(write.params.name,'orbit_task_update');
  assert.equal(write.params.arguments.expected_revision,'rev1');
  for(const timer of [...f.timers.values()])timer();
  await flush();
  assert.equal(f.get('comment').value,'My preserved comment');
  f.click('refresh');
  assert.deepEqual(f.last().params.arguments,write.params.arguments);
  f.answer(f.last(),{
    snapshot:f.detail().snapshot,replayed:true
  });
  await flush();
  assert.equal(f.get('comment').value,'');
});
test('changes requested is evidence-bound and never terminal rejection; review uses criterion outcomes',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('evidence').value='artifact:checks';
  f.get('rationale').value='Fix missing test';
  f.click('changes');
  const op=f.last().params.arguments;
  assert.equal(f.last().params.name,'orbit_task_update');
  assert.equal(op.verdict.decision,'changes_requested');
  assert.equal(op.complete,false);
  assert.equal(op.verdict.criteria[0].criterion,'Works');
  assert.equal(op.verdict.criteria[0].met,false);
  assert.deepEqual([...op.verdict.evidence],['artifact:checks']);
  assert.ok(!f.posted.some(m=>m.params?.name==='orbit_task_reject'));
});
test('runs fixture bounds 100 rows and retains unavailable usage',async()=>{
  const f=fixture();
  await f.init();
  f.click('runs');
  f.answer(f.last(),f.list(Array.from({
    length:100
  },(_,i)=>({
    id:`jrun-${i}`,state:'failed'
  }))));
  await flush();
  assert.equal(f.get('list').querySelectorAll('button').length,50);
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),{
    schema_version:1,workspace:'host-a/ws_shared',scope:'run',run:{
      id:'jrun-0',state:'failed',usage:{
        state:'unavailable'
      },steps:[]
    },logs:{
      items:[{
        stdout:'<script>evil()</script>'
      }],pagination:{
        next_offset:null
      }
    }
  });
  await flush();
  assert.ok(f.get('details').children.some(n=>n.children[1].textContent.includes('unavailable')));
  assert.equal(f.get('comment-form').hidden,true);
});
test('create and edit forms send exact typed fields; closing editor retains draft',async()=>{
  const f=fixture();
  await f.init();
  f.click('create');
  f.get('draft-title').value='Captured task';
  f.get('draft-description').value='Useful description';
  f.get('draft-criteria').value='Criterion one\nCriterion two';
  f.get('draft-priority').value='high';
  f.get('draft-crew').value='crew_sol';
  f.click('cancel-edit');
  f.click('create');
  assert.equal(f.get('draft-title').value,'Captured task');
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const create=f.last();
  assert.equal(create.params.name,'orbit_task_add');
  assert.equal(create.params.arguments.priority,'high');
  assert.deepEqual([...create.params.arguments.acceptance_criteria],['Criterion one','Criterion two']);
  f.answer(create,{
    snapshot:f.detail().snapshot,replayed:false
  });
  await flush();
  const readRequests=f.posted.filter(m=>['orbit_task_list','orbit_task_show','orbit_workflow_run_list','orbit_workflow_run_show'].includes(m.params?.name)).slice(-2);
  for(const request of readRequests)f.answer(request,request.params.name==='orbit_task_show'?f.detail():f.list());
  await flush();
  f.click('edit');
  f.get('draft-title').value='Changed title';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const edit=f.last().params.arguments;
  assert.equal(f.last().params.name,'orbit_task_update');
  assert.equal(edit.expected_revision,'rev1');
  assert.equal(edit.title,'Changed title');
  assert.ok(!('fields' in edit));
});
test('server action refusal disables completion; generic errors retain retry identity and draft',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.actions.complete={
    enabled:false,reason:'Active run must stop'
  };
  f.answer(f.last(),d);
  await flush();
  assert.equal(f.get('accept').disabled,true);
  assert.equal(f.get('accept').title,'Active run must stop');
  f.get('comment').value='Preserve on conflict';
  f.get('comment-form').handlers.submit({
    preventDefault(){
    }
  });
  f.answer(f.last(),{
    message:'Snapshot read failed after mutation'
  },true);
  await flush();
  assert.equal(f.get('comment').value,'Preserve on conflict');
  assert.equal(f.get('comment-submit').disabled,true);
  const original=f.last();
  f.click('refresh');
  assert.equal(f.last().params.name,'orbit_task_update');
  assert.deepEqual(f.last().params.arguments,original.params.arguments);
});
test('comment pagination and logs pagination carry independent bounded offsets',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.comments_pagination={
    truncated:true,next_offset:50
  };
  f.answer(f.last(),d.snapshot);
  await flush();
  assert.equal(f.get('more-comments').hidden,false);
  f.click('more-comments');
  assert.equal(f.last().params.arguments.comments_offset,50);
  assert.equal(f.last().params.arguments.limit,50);
  f.click('runs');
  const pending=f.posted.filter(m=>m.params?.name==='orbit_workflow_run_list').at(-1);
  f.answer(pending,f.list([{
    id:'jrun-1',state:'running'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),{
    schema_version:1,workspace:'host-a/ws_shared',run:{
      id:'jrun-1',state:'running'
    },logs:{
      items:[],pagination:{
        next_offset:50,truncated:true
      }
    }
  });
  await flush();
  f.click('more-logs');
  assert.equal(f.last().params.arguments.log_offset,50);
});
test('filter coverage is explicit and full history uses the history tool',async()=>{
  const f=fixture();
  await f.init();
  f.get('query').value='needle';
  f.get('priority').value='high';
  f.click('review');
  f.get('filters').handlers.submit({
    preventDefault(){
    }
  });
  const args=f.last().params.arguments;
  assert.equal(args.search,'needle');
  assert.equal(args.priority,'high');
  assert.equal(args.status,'review');
  f.click('history');
  assert.equal(f.last().params.name,'orbit_search');
  assert.equal(f.last().params.arguments.workspace,'host-a/ws_shared');
  assert.equal(f.last().params.arguments.limit,50);
});
test('trusted teardown acknowledges and prevents racing initialization; untrusted notifications have no effect',async()=>{
  const f=fixture();
  f.receive({
    source:{
    },data:{
      jsonrpc:'2.0',method:'ui/notifications/tool-result',params:{
        structuredContent:{
          schema_version:1,workspace:'attacker',task:{
            id:'ORB-evil'
          }
        }
      }
    }
  });
  assert.equal(f.posted.length,1);
  f.receive({
    source:f.parent,data:{
      jsonrpc:'2.0',id:900,method:'ui/resource-teardown'
    }
  });
  assert.equal(f.last().id,900);
  assert.ok('result'in f.last());
  f.answer(f.posted[0],{
    protocolVersion:'2026-01-26',hostCapabilities:{
      serverTools:{
      }
    }
  });
  await flush();
  assert.ok(!f.posted.some(m=>m.params?.name==='orbit_workspace_list'));
  assert.equal(f.get('refresh').disabled,true);
});
test('run tool notifications preserve conversation entity identity and bounded context',async()=>{
  const f=fixture();
  await f.init();
  f.receive({
    source:f.parent,data:{
      jsonrpc:'2.0',method:'ui/notifications/tool-result',params:{
        structuredContent:{
          schema_version:1,workspace:'host-a/ws_shared',kind:'run',run:{
            id:'jrun-selected'
          }
        }
      }
    }
  });
  const detailRequest=f.last();
  assert.equal(detailRequest.params.name,'orbit_workflow_run_show');
  assert.equal(detailRequest.params.arguments.id,'jrun-selected');
  f.answer(detailRequest,{
    schema_version:1,workspace:'host-a/ws_shared',run:{
      id:'jrun-selected',state:'stopped'
    },logs:{
      items:[],pagination:{
        next_offset:null
      }
    }
  });
  await flush();
  f.click('send');
  const ref=JSON.parse(f.last().params.content[0].text);
  assert.equal(ref.entity.kind,'run');
  assert.equal(ref.entity.id,'jrun-selected');
});
test('refreshing a changed task never silently rebases an edit or verdict draft',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-title').value='Keep this edit';
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Reviewed original evidence';
  f.click('refresh');
  const requests=f.posted.filter(m=>['orbit_task_list','orbit_task_show','orbit_workflow_run_list','orbit_workflow_run_show'].includes(m.params?.name)).slice(-2);
  for(const request of requests)f.answer(request,request.params.name==='orbit_task_show'?f.detail('ORB-1','rev2'):f.list());
  await flush();
  assert.equal(f.get('draft-title').value,'Keep this edit');
  assert.equal(f.get('save').disabled,true);
  assert.equal(f.get('accept').disabled,true);
  assert.equal(f.get('changes').disabled,true);
  assert.equal(f.get('rationale').value,'Reviewed original evidence');
  f.click('edit');
  assert.equal(f.get('save').disabled,false);
  assert.equal(f.get('draft-title').value,'Keep this edit');
});
test('wrong destination responses preserve good rows and mark current state stale',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Good'
  }]));
  await flush();
  f.click('refresh');
  const wrong=f.list([{
    id:'ORB-evil',title:'Wrong host'
  }]);
  wrong.workspace='host-b/ws_shared';
  f.answer(f.last(),wrong);
  await flush();
  assert.equal(f.get('list').querySelectorAll('button')[0].dataset.entityKey,'ORB-1');
  assert.equal(f.get('list').querySelectorAll('button')[0].children[0].textContent,'Good');
  assert.ok(f.get('state').textContent.includes('Stale'));
});
test('missing model-context capability keeps a self-contained copyable reference',async()=>{
  const f=fixture();
  await f.init({
    serverTools:{
    }
  });
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('send');
  await flush();
  assert.ok(!f.posted.some(m=>m.method==='ui/update-model-context'));
  const ref=JSON.parse(f.get('reference').textContent);
  assert.equal(ref.entity.id,'ORB-1');
  assert.equal(ref.workspace,'host-a/ws_shared');
  assert.equal(f.get('copy').hidden,false);
});
test('review binds observed PR head and disables a draft when that head changes',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.reviewed_head='head-a';
  d.snapshot.task.job_run_id='jrun-evidence';
  f.answer(f.last(),d);
  await flush();
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Checked';
  f.click('changes');
  const verdict=f.last().params.arguments.verdict;
  assert.equal(verdict.expected_head,'head-a');
  assert.equal(verdict.expected_run_id,'jrun-evidence');
  f.answer(f.last(),{
    workspace:'host-a/ws_shared',conflict:{
      code:'revision_conflict',message:'Evidence changed'
    },snapshot:d.snapshot
  });
  await flush();
  f.click('refresh');
  const requests=f.posted.filter(m=>['orbit_task_list','orbit_task_show','orbit_workflow_run_list','orbit_workflow_run_show'].includes(m.params?.name)).slice(-2);
  const changed=f.detail();
  changed.snapshot.reviewed_head='head-b';
  for(const request of requests)f.answer(request,request.params.name==='orbit_task_show'?changed:f.list());
  await flush();
  assert.equal(f.get('changes').disabled,true);
  assert.equal(f.get('accept').disabled,true);
});
test('task navigation preserves editor identity and per-task review outcomes and original revision',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'One'
  },{
    id:'ORB-2',title:'Two'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-title').value='First entity draft';
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Checked first';
  f.get('criterion-outcomes').querySelectorAll('select')[0].value='met';
  f.get('list').querySelectorAll('button')[1].handlers.click();
  f.answer(f.last(),f.detail('ORB-2','second-revision'));
  await flush();
  assert.equal(f.get('editor').hidden,true);
  assert.equal(f.get('evidence').value,'');
  f.click('edit');
  assert.notEqual(f.get('draft-title').value,'First entity draft');
  f.get('draft-title').value='Second entity draft';
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail('ORB-1','changed-first'));
  await flush();
  assert.equal(f.get('rationale').value,'Checked first');
  assert.equal(f.get('criterion-outcomes').querySelectorAll('select')[0].value,'met');
  assert.equal(f.get('accept').disabled,true);
  f.click('edit');
  assert.equal(f.get('draft-title').value,'First entity draft');
});
test('workspace navigation saves create draft under the original opaque destination',async()=>{
  const f=fixture();
  await f.init();
  f.click('create');
  f.get('draft-title').value='Host A draft';
  f.get('workspace').value='host-b/ws_shared';
  f.get('workspace').handlers.change();
  f.answer(f.last(),{
    ...f.list(),workspace:'host-b/ws_shared'
  });
  await flush();
  f.click('create');
  assert.equal(f.get('draft-title').value,'');
  f.get('draft-title').value='Host B draft';
  f.get('workspace').value='host-a/ws_shared';
  f.get('workspace').handlers.change();
  f.answer(f.last(),f.list());
  await flush();
  f.click('create');
  assert.equal(f.get('draft-title').value,'Host A draft');
});
test('structured conflict displays fresh state but preserves the original edit draft revision',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-title').value='My edited title';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  f.answer(f.last(),{
    workspace:'host-a/ws_shared',conflict:{
      code:'revision_conflict',message:'Changed by another client'
    },snapshot:f.detail('ORB-1','fresh-revision').snapshot
  });
  await flush();
  assert.equal(f.get('draft-title').value,'My edited title');
  assert.equal(f.get('editor').hidden,false);
  assert.equal(f.get('save').disabled,true);
  assert.ok(f.get('identity').title.includes('fresh-revision'));
  assert.ok(f.get('state').textContent.includes('Write refused'));
  f.click('refresh');
  assert.equal(f.last().params.name,'orbit_task_show');
});
test('late write completion cannot retarget a different entity or erase newly typed comment',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'One'
  },{
    id:'ORB-2',title:'Two'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('comment').value='Submitted';
  f.get('comment-form').handlers.submit({
    preventDefault(){
    }
  });
  const write=f.last();
  f.get('list').querySelectorAll('button')[1].handlers.click();
  f.answer(f.last(),f.detail('ORB-2','rev2'));
  await flush();
  f.get('comment').value='Second task draft';
  f.answer(write,{
    snapshot:f.detail().snapshot,replayed:false
  });
  await flush();
  assert.ok(f.get('identity').textContent.includes('ORB-2'));
  assert.equal(f.get('comment').value,'Second task draft');
  const requests=f.posted.filter(m=>['orbit_task_list','orbit_task_show','orbit_workflow_run_list','orbit_workflow_run_show'].includes(m.params?.name)).slice(-2);
  for(const request of requests)f.answer(request,request.params.name==='orbit_task_show'?f.detail('ORB-2','rev2'):f.list([{
    id:'ORB-1',title:'One'
  },{
    id:'ORB-2',title:'Two'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  assert.equal(f.get('comment').value,'');
});
test('typing during a successful edit retains the newer unsent draft',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-title').value='Submitted title';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const write=f.last();
  f.get('draft-title').value='New unsent title';
  f.answer(write,{
    snapshot:f.detail('ORB-1','rev2').snapshot,replayed:false
  });
  await flush();
  assert.equal(f.get('draft-title').value,'New unsent title');
  assert.equal(f.get('editor').hidden,false);
  assert.equal(f.get('save').disabled,true);
});
test('unapplied filter text does not silently change a polling request',async()=>{
  const f=fixture();
  await f.init();
  f.get('query').value='Not applied';
  f.click('refresh');
  assert.ok(!('search' in f.last().params.arguments));
  f.get('filters').handlers.submit({
    preventDefault(){
    }
  });
  assert.equal(f.last().params.arguments.search,'Not applied');
  f.answer(f.last(),{
    ...f.list(),items:[],pagination:{
      offset:0,limit:50,truncated:true,next_offset:null
    }
  });
  await flush();
  assert.equal(f.get('next').disabled,true);
});
test('a truncated final log page exposes truncation without an endless next page',async()=>{
  const f=fixture();
  await f.init();
  f.click('runs');
  f.answer(f.last(),f.list([{
    id:'jrun-1',state:'failed'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),{
    schema_version:1,workspace:'host-a/ws_shared',run:{
      id:'jrun-1',state:'failed'
    },logs:{
      items:[{
        stdout:'excerpt',stdout_truncated:true
      }],pagination:{
        truncated:true,next_offset:null
      }
    }
  });
  await flush();
  assert.equal(f.get('more-logs').hidden,true);
  assert.ok(f.get('details').children.some(n=>n.children[1].textContent.includes('stdout truncated')));
});
test('disposed editor cannot submit even when a form event is dispatched directly',async()=>{
  const f=fixture();
  await f.init();
  f.click('create');
  f.get('draft-title').value='Must not submit';
  f.get('draft-criteria').value='Criterion';
  f.receive({
    source:f.parent,data:{
      jsonrpc:'2.0',id:44,method:'ui/resource-teardown'
    }
  });
  const count=f.posted.length;
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  assert.equal(f.posted.length,count);
  assert.equal(f.get('save').disabled,true);
});
test('unknown notification destination is refused without falling back or sending reads',async()=>{
  const f=fixture();
  await f.init();
  const count=f.posted.length;
  f.receive({
    source:f.parent,data:{
      jsonrpc:'2.0',method:'ui/notifications/tool-result',params:{
        structuredContent:{
          schema_version:1,workspace:'unknown/ws_shared',task:{
            id:'ORB-1'
          }
        }
      }
    }
  });
  await flush();
  assert.equal(f.posted.length,count);
  assert.equal(f.get('create').disabled,true);
  assert.ok(f.get('state').textContent.includes('not discovered'));
});
test('refresh preserves criterion control identity for keyboard focus and focused list row',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  const criterion=f.get('criterion-outcomes').querySelectorAll('select')[0];
  f.document.activeElement=f.get('list').querySelectorAll('button')[0];
  f.click('refresh');
  const requests=f.posted.filter(m=>['orbit_task_list','orbit_task_show','orbit_workflow_run_list','orbit_workflow_run_show'].includes(m.params?.name)).slice(-2);
  for(const request of requests)f.answer(request,request.params.name==='orbit_task_show'?f.detail():f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  assert.equal(f.get('criterion-outcomes').querySelectorAll('select')[0],criterion);
  assert.equal(f.get('list').querySelectorAll('button')[0].focused,true);
});
test('chat context bounds task text and references while retaining authoritative entity identity',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.task.title='x'.repeat(100000);
  d.snapshot.task.description='complete logs'.repeat(100000);
  d.snapshot.artifacts=Array.from({
    length:1000
  },(_,i)=>({
    path:`artifact-${i}`
  }));
  d.snapshot.task.job_run_id='jrun-evidence';
  f.answer(f.last(),d);
  await flush();
  f.click('send');
  const text=f.last().params.content[0].text;
  const context=JSON.parse(text);
  assert.ok(text.length<5000);
  assert.equal(context.title.length,500);
  assert.equal(context.evidence.length,10);
  assert.equal(context.evidence[0],'artifact-0');
  assert.ok(!text.includes('complete logs'));
  assert.equal(context.entity.id,'ORB-1');
});
test('truncated task projection warns and honours disabled backend actions',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.content_truncated=true;
  d.snapshot.truncated_fields=['description'];
  d.snapshot.actions.edit={
    enabled:false,reason:'Incomplete task evidence'
  };
  d.snapshot.actions.review={
    enabled:false,reason:'Incomplete task evidence'
  };
  d.snapshot.actions.complete={
    enabled:false,reason:'Incomplete task evidence'
  };
  f.answer(f.last(),d);
  await flush();
  assert.equal(f.get('projection-warning').hidden,false);
  assert.ok(f.get('projection-warning').textContent.includes('description'));
  assert.equal(f.get('edit').disabled,true);
  assert.equal(f.get('changes').disabled,true);
  assert.equal(f.get('accept').disabled,true);
});
test('record-only acceptance is available with review authority and never asks for completion',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.actions.complete={
    enabled:false,reason:'Operator capability required'
  };
  f.answer(f.last(),d);
  await flush();
  assert.equal(f.get('accept').disabled,true);
  assert.equal(f.get('record-accept').disabled,false);
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Criteria checked';
  f.get('criterion-outcomes').querySelectorAll('select')[0].value='met';
  f.click('record-accept');
  const op=f.last().params.arguments;
  assert.equal(op.verdict.decision,'accept');
  assert.equal(op.complete,false);
  assert.equal(op.verdict.criteria[0].met,true);
  assert.equal(op.expected_revision,'rev1');
});
test('bounded task rows visibly disclose truncation and omitted run identity',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Clipped title',title_truncated:true,crew:'Clipped crew',crew_truncated:true,relations:[],relations_total:80,relations_truncated:true,dependencies:[],dependencies_total:70,dependencies_truncated:true,job_run_id_omitted:true
  }]));
  await flush();
  const summary=f.get('list').querySelectorAll('button')[0].textContent;
  assert.ok(summary.includes('Truncated: title, crew'));
  assert.ok(summary.includes('of 80'));
  assert.ok(summary.includes('of 70'));
  assert.ok(summary.includes('Run reference omitted'));
});
test('a failed current read clears the reference and copy action without claiming host context was revoked',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('send');
  f.answer(f.last(),{
  });
  await flush();
  assert.equal(f.get('copy').hidden,false);
  assert.ok(f.get('reference').textContent);
  f.click('refresh');
  f.answer(f.last(),{
    message:'Offline'
  },true);
  await flush();
  assert.equal(f.get('reference').textContent,'');
  assert.equal(f.get('copy').hidden,true);
  assert.equal(f.get('send').disabled,true);
  assert.ok(f.get('state').textContent.includes('Previously sent context remains in chat'));
});
test('review comment JSON is readable literal text without granting action authority',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  d.snapshot.actions.complete={
    enabled:false,reason:'No operator authority'
  };
  const verdict={
    decision:'accept',rationale:'<script>evil()</script>',criteria:[{
      criterion:'Works',met:true,evidence:['<img onerror=evil()>']
    }]
  };
  d.comments={
    items:[{
      comment_id:'private-storage-id',at:'now',by:'<img onerror=evil()>',body:`desktop_review_verdict=${JSON.stringify(verdict)}\nreviewed_revision=old-revision`
    }],pagination:{
      next_offset:null
    }
  };
  f.answer(f.last(),d);
  await flush();
  const fields=f.get('details').children;
  const comment=fields.find(n=>n.children[0].textContent.startsWith('Comment ·'));
  assert.ok(comment.children[1].textContent.includes('"decision": "accept"'));
  assert.ok(comment.children[1].textContent.includes('<script>evil()</script>'));
  assert.ok(comment.children[1].textContent.includes('reviewed_revision=old-revision'));
  assert.ok(!comment.children[1].textContent.includes('private-storage-id'));
  assert.ok(fields.some(n=>n.children[1].textContent.includes('Recorded review comments below')));
  assert.equal(f.get('accept').disabled,true);
});
test('malformed verdict JSON and ordinary comments preserve their literal body',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  const d=f.detail();
  const malformed='desktop_review_verdict={broken <svg onload=evil()>\nrequest_id=original';
  const ordinary='<a href="javascript:evil()">click</a>';
  d.comments={
    items:[{
      at:'now',by:'user',body:malformed
    },{
      at:'later',by:'user',message:ordinary
    }],pagination:{
      next_offset:null
    }
  };
  f.answer(f.last(),d);
  await flush();
  const comments=f.get('details').children.filter(n=>n.children[0].textContent.startsWith('Comment ·'));
  assert.equal(comments.length,2);
  assert.equal(comments[0].children[1].textContent,malformed);
  assert.equal(comments[1].children[1].textContent,ordinary);
});
test('proven precommit refusal preserves editable draft and permits correction with a fresh identity',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-crew').value='missing-crew';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const rejected=f.last();
  f.answer(rejected,{
    refusal:{
      code:'invalid_crew',message:'Crew is not registered'
    },mutation_applied:false
  });
  await flush();
  assert.equal(f.get('draft-crew').value,'missing-crew');
  assert.equal(f.get('editor').hidden,false);
  assert.equal(f.get('save').disabled,false);
  assert.equal(f.get('comment-submit').disabled,false);
  assert.ok(f.get('state').textContent.includes('Crew is not registered'));
  f.get('draft-crew').value='crew_sol';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const corrected=f.last();
  assert.notEqual(corrected.params.arguments.request_id,rejected.params.arguments.request_id);
  assert.equal(corrected.params.arguments.crew,'crew_sol');
  assert.equal(corrected.params.arguments.expected_revision,'rev1');
});
test('a refusal without explicit no-mutation proof remains an ambiguous outcome and retains identity',async()=>{
  const f=fixture();
  await f.init();
  f.click('create');
  f.get('draft-title').value='Captured';
  f.get('draft-criteria').value='Criterion';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const original=f.last();
  f.answer(original,{
    refusal:{
      code:'unexpected',message:'Could follow a committed write'
    }
  });
  await flush();
  assert.equal(f.get('draft-title').value,'Captured');
  assert.equal(f.get('save').disabled,true);
  f.click('refresh');
  assert.deepEqual(f.last().params.arguments,original.params.arguments);
});
test('canonical run projection displays separate timestamps and step truncation alongside usage and logs',async()=>{
  const f=fixture();
  await f.init();
  f.click('runs');
  f.answer(f.last(),f.list([{
    id:'jrun-real',run_id:'jrun-real',job_id:'ship',state:'failed',created_at:'2026-10-03T01:00:00Z',started_at:'2026-10-03T01:01:00Z'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),{
    schema_version:1,workspace:'host-a/ws_shared',observed_at:'2026-10-03T01:10:00Z',scope:'run',run:{
      id:'jrun-real',run_id:'jrun-real',job_id:'ship',state:'failed',attempt:1,scheduled_at:'2026-10-03T00:59:00Z',created_at:'2026-10-03T01:00:00Z',started_at:'2026-10-03T01:01:00Z',finished_at:'2026-10-03T01:09:00Z',duration_ms:480000,steps:Array.from({
        length:50
      },(_,i)=>({
        step_index:i,target_id:'ORB-1',target_type:'task',state:'failed',error_code:'fixture_failed',error_message:'<script>evil()</script>'
      })),steps_total:70,steps_truncated:true,usage:{
        state:'unavailable'
      }
    },usage:{
      state:'unavailable'
    },execution_progress:{
      state:'observed',active_step:null,provider_processes:{
        limit:50,truncated:false,items:[]
      }
    },logs:{
      state:'observed',items:[{
        event_id:'event',ts:'2026-10-03T01:09:00Z',step_id:'step',step_index:0,provider:'fixture',stdout:'<img onerror=evil()>',stderr:'fixture error',stdout_truncated:true,stderr_truncated:false,exit_code:1,timed_out:false,duration_ms:1
      }],total:1,pagination:{
        offset:0,limit:50,truncated:false,next_offset:null
      },available_through:1,excerpt_max_bytes:4096
    }
  });
  await flush();
  const fields=f.get('details').children;
  const timestamps=fields.find(n=>n.children[0].textContent==='Timestamps').children[1].querySelectorAll('dd');
  assert.deepEqual(timestamps.map(n=>n.title),['2026-10-03T00:59:00Z','2026-10-03T01:00:00Z','2026-10-03T01:01:00Z','2026-10-03T01:09:00Z'],'full timestamps remain available as tooltips');
  assert.ok(timestamps.every(n=>n.textContent && n.textContent!==n.title),'visible timestamps use readable local dates');
  assert.equal(f.get('title').textContent,'ship');
  const coverage=fields.find(n=>n.children[0].textContent==='Step coverage').children[1].textContent;
  assert.ok(coverage.includes('50 steps shown of 70'));
  assert.ok(coverage.includes('Truncated'));
  assert.ok(fields.find(n=>n.children[0].textContent==='Cost / usage').children[1].textContent.includes('unavailable'));
  assert.ok(fields.find(n=>n.children[0].textContent==='Log excerpts').children[1].textContent.includes('<img onerror=evil()>'));
  assert.equal(f.get('more-logs').hidden,true);
});
test('accepted write with failed snapshot remains successful and reconciles the same identity without another effect',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('comment').value='One accepted comment';
  f.get('comment-form').handlers.submit({
    preventDefault(){
    }
  });
  const original=f.last();
  let effects=0;
  const receipts=new Set();
  const apply=request=>{
    const key=request.params.arguments.request_id;
    if(!receipts.has(key)){
      receipts.add(key);
      effects++;
    }
  };
  apply(original);
  f.answer(original,{
    accepted:true,task_id:'ORB-1',workspace:'host-a/ws_shared',refresh_error:'Snapshot currently unreadable'
  });
  await flush();
  assert.equal(effects,1);
  assert.equal(f.get('comment').value,'One accepted comment');
  assert.equal(f.get('comment-submit').disabled,true);
  assert.equal(f.get('write-state').hidden,false);
  assert.ok(f.get('write-state').textContent.includes('Write succeeded; refresh unavailable'));
  assert.ok(!f.get('state').textContent.includes('outcome unknown'));
  f.click('refresh');
  const retry=f.last();
  assert.deepEqual(retry.params.arguments,original.params.arguments);
  apply(retry);
  f.answer(retry,{
    snapshot:f.detail('ORB-1','after-comment').snapshot,replayed:true
  });
  await flush();
  assert.equal(effects,1);
  assert.equal(f.get('comment').value,'');
  assert.ok(f.get('write-state').textContent.includes('one accepted effect'));
});
test('a later generic reconcile error cannot turn a proven accepted write into an unknown outcome',async()=>{
  const f=fixture();
  await f.init();
  f.click('create');
  f.get('draft-title').value='Accepted proposal';
  f.get('draft-criteria').value='Criterion';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const original=f.last();
  f.answer(original,{
    accepted:true,task_id:'ORB-new',workspace:'host-a/ws_shared',refresh_error:'Temporary read failure'
  });
  await flush();
  f.click('refresh');
  f.answer(f.last(),{
    message:'Transport failure'
  },true);
  await flush();
  assert.ok(f.get('write-state').textContent.includes('Write succeeded'));
  assert.ok(f.get('state').textContent.includes('Write succeeded'));
  assert.ok(!f.get('state').textContent.includes('outcome unknown'));
  assert.equal(f.get('draft-title').value,'Accepted proposal');
  assert.equal(f.get('save').disabled,true);
  f.click('refresh');
  assert.deepEqual(f.last().params.arguments,original.params.arguments);
});
test('canonical run rows show observed state attempt and duration without treating missing duration as zero',async()=>{
  const f=fixture();
  await f.init();
  f.click('runs');
  f.answer(f.last(),f.list([{
    id:'jrun-failed',run_id:'jrun-failed',state:'failed',attempt:2,duration_ms:1250,created_at:'2026-10-03T01:00:00Z'
  },{
    id:'jrun-running',run_id:'jrun-running',state:'running',attempt:1,duration_ms:null,created_at:'2026-10-03T01:00:00Z'
  }]));
  await flush();
  const failed=f.get('list').querySelectorAll('button')[0].textContent;
  assert.ok(failed.includes('failed'));
  assert.ok(failed.includes('Attempt 2'));
  assert.ok(failed.includes('1s'));
  assert.equal(f.get('list').querySelectorAll('button')[0].children[0].textContent,'Workflow run');
  const running=f.get('list').querySelectorAll('button')[1].textContent;
  assert.ok(running.includes('running'));
  assert.ok(running.includes('Duration unavailable'));
  assert.ok(!running.includes('Duration 0 ms'));
});

test('a different selected entity clears prior detail through a failed read and retry',async()=>{
  const f=fixture();await f.init();f.click('runs');
  f.answer(f.last(),f.list([{id:'jrun-one',title:'First run'},{id:'jrun-two',title:'Second run'}]));await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click();
  f.answer(f.last(),{schema_version:1,workspace:'host-a/ws_shared',run:{id:'jrun-one',title:'First run',state:'running',steps:[{step_id:'old-step'}]}});await flush();
  assert.equal(f.get('title').textContent,'First run');
  f.get('list').querySelectorAll('button')[1].handlers.click();
  assert.equal(f.get('title').textContent,'jrun-two');
  assert.match(f.get('identity').textContent,/Loading/);
  assert.ok(!f.get('details').textContent.includes('old-step'));
  assert.equal(f.get('send').disabled,true);
  f.answer(f.last(),{message:'Selected run read refused'},true);await flush();
  assert.equal(f.get('title').textContent,'jrun-two');
  assert.match(f.get('identity').textContent,/unavailable.*Refresh.*retry/);
  assert.equal(f.get('entity-status').children.length,0);
  assert.equal(f.get('details').children.length,0);
  assert.equal(f.get('send').disabled,true);
  f.click('refresh');
  const requests=f.posted.filter(m=>m.method==='tools/call').slice(-2);
  for(const request of requests)f.answer(request,request.params.name==='orbit_workflow_run_list'?f.list([{id:'jrun-one'},{id:'jrun-two'}]):{schema_version:1,workspace:'host-a/ws_shared',run:{id:'jrun-two',title:'Second run',state:'success'}});
  await flush();
  assert.equal(f.get('title').textContent,'Second run');
  assert.equal(f.get('send').disabled,false);
});

test('switching views clears unrelated detail and stale rows while preserving comment drafts', async()=>{
  const f=fixture(); await f.init();
  f.click('refresh'); f.answer(f.last(),f.list([{id:'ORB-1',title:'One'}])); await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click(); f.answer(f.last(),f.detail()); await flush();
  f.get('comment').value='unsent comment';
  f.click('runs');
  assert.equal(f.get('panel').hidden,true);
  assert.equal(f.get('list').children.length,0);
  assert.equal(f.last().params.name,'orbit_workflow_run_list');
  assert.ok(!f.posted.slice(-1).some(m=>m.params?.name==='orbit_task_show'));
  f.answer(f.last(),f.list()); await flush();
  f.click('tasks'); f.answer(f.last(),f.list([{id:'ORB-1',title:'One'}])); await flush();
  f.get('list').querySelectorAll('button')[0].handlers.click(); f.answer(f.last(),f.detail()); await flush();
  assert.equal(f.get('comment').value,'unsent comment');
});
test('task status selection covers active work, expanded sets and All before grouping and pagination',async()=>{
  const f=fixture(); await f.init();
  const active=['in-progress','review','blocked','backlog'];
  const terminal=['done','rejected','archived'];
  const excluded=['proposed',...terminal,'someday'];
  // More than a page of excluded newest tasks guards against browser filtering
  // a mixed first page and silently losing matching older work.
  const tasks=[
    ...Array.from({length:65},(_,i)=>({id:`closed-${i}`,title:'Needle hidden',status:excluded[i%excluded.length],priority:'high'})),
    ...Array.from({length:61},(_,i)=>({id:`active-${i}`,title:`Needle active ${i}`,status:active[i%active.length].replaceAll('-','_'),priority:'high'})),
    ...Array.from({length:4},(_,i)=>({id:`low-${i}`,title:'Needle low',status:'backlog',priority:'low'})),
    ...Array.from({length:5},(_,i)=>({id:`other-${i}`,title:'Other title',status:'backlog',priority:'high'})),
  ];
  const inputs=()=>f.get('task-status-options').querySelectorAll('input');
  const included=()=>inputs().filter(input=>input.checked).map(input=>input.value).sort();
  const headings=()=>f.get('list').querySelectorAll('h3').map(h=>h.dataset.status);
  const answerList=async()=>{
    const request=f.last();
    assert.equal(request.params.name,'orbit_task_list');
    const a=request.params.arguments;
    const statuses=a.status?.split(',');
    const matches=tasks.filter(t=>(!statuses||statuses.includes(t.status.replaceAll('_','-')))&&(!a.priority||t.priority===a.priority)&&(!a.search||(t.id+' '+t.title).toLowerCase().includes(a.search.toLowerCase())));
    const items=matches.slice(a.offset,a.offset+a.limit);
    f.answer(request,{...f.list(items),total:matches.length,pagination:{total:matches.length,offset:a.offset,limit:a.limit,next_offset:a.offset+items.length<matches.length?a.offset+items.length:null}});
    await flush();
    assert.equal(f.get('list').querySelectorAll('button').length,items.length);
    return {a,items,total:matches.length};
  };
  assert.deepEqual(f.last().params.arguments.status.split(',').sort(),active.slice().sort(),'The explicit desktop default excludes proposed and terminal tasks');
  assert.deepEqual(included(),active.slice().sort());
  assert.equal(f.get('active-statuses')['aria-pressed'],'true');
  f.click('refresh');
  assert.equal((await answerList()).total,70);
  assert.deepEqual(headings(),['review','blocked','in-progress','backlog']);
  const firstGroup=f.get('list').querySelectorAll('h3')[0];
  assert.equal(firstGroup.children[1].textContent,'13');
  assert.ok(f.get('list').querySelectorAll('button').every(row=>row.dataset.entityKey.startsWith('active-')));
  f.get('query').value='needle'; f.get('priority').value='high';
  f.get('filters').handlers.submit({preventDefault(){}});
  let page=await answerList();
  assert.equal(page.total,61);
  assert.equal(page.a.search,'needle'); assert.equal(page.a.priority,'high');
  assert.match(f.get('pagination').textContent,/1–50 of 61/);
  f.click('next'); page=await answerList();
  assert.equal(page.a.offset,50); assert.equal(page.items.length,11);
  assert.match(f.get('pagination').textContent,/51–61 of 61/);
  assert.equal(f.get('next').disabled,true);
  f.click('previous'); assert.equal((await answerList()).a.offset,0);
  for(const status of ['proposed','done']){
    const input=inputs().find(input=>input.value===status);
    input.checked=true; input.handlers.change();
    page=await answerList(); assert.equal(page.a.offset,0);
  }
  assert.equal(page.total,87);
  assert.deepEqual(included(),[...active,'proposed','done'].sort());
  assert.deepEqual(headings(),['proposed','review','blocked','in-progress','backlog','done']);
  const order=f.get('list').querySelectorAll('button').map(row=>row.dataset.entityKey);
  assert.deepEqual(order.slice(0,13),Array.from({length:13},(_,i)=>`closed-${i*5}`),'Grouping preserves server order within each status');
  f.click('refresh'); page=await answerList();
  assert.equal(page.total,87); assert.equal(page.a.search,'needle'); assert.equal(page.a.priority,'high');
  f.click('next'); page=await answerList(); assert.equal(page.total,87); assert.equal(page.a.offset,50);
  f.click('all-statuses'); page=await answerList();
  assert.ok(!('status' in page.a)); assert.equal(page.total,126); assert.equal(page.a.offset,0);
  assert.equal(f.get('all-statuses')['aria-pressed'],'true');
  assert.deepEqual(included(),[...active,...excluded].sort());
  assert.deepEqual(headings(),['proposed','someday','done','rejected','archived']);
  f.click('review'); assert.equal(f.last().params.arguments.status,'review');
  f.answer(f.last(),f.list()); await flush();
  assert.equal(f.get('task-status-filter').hidden,true);
  f.click('runs'); assert.equal(f.last().params.name,'orbit_workflow_run_list');
  assert.ok(!('status' in f.last().params.arguments));
  f.answer(f.last(),f.list()); await flush();
  f.click('tasks'); page=await answerList(); assert.ok(!('status' in page.a));
  f.click('active-statuses'); await answerList();
  assert.deepEqual(included(),active.slice().sort());
  const row=f.get('list').querySelectorAll('button')[0];
  assert.equal(row.tag,'button'); assert.equal(row.type,'button');
  f.document.activeElement=row;
  f.click('refresh'); await answerList();
  assert.equal(f.get('list').querySelectorAll('button')[0].focused,true);
  row.handlers.click();
  assert.equal(f.last().params.arguments.id,row.dataset.entityKey);
  f.answer(f.last(),f.detail(row.dataset.entityKey)); await flush();
  assert.equal(f.get('panel').hidden,false); assert.equal(f.get('title').focused,true);
  f.click('close');
  assert.ok(f.get('list').querySelectorAll('button').every(row=>row['aria-expanded']==='false'));
  f.get('query').value='no such task'; f.get('filters').handlers.submit({preventDefault(){}});
  page=await answerList(); assert.equal(page.total,0);
  assert.deepEqual(headings(),[]); assert.equal(f.get('next').disabled,true);
  assert.ok(f.get('list').children[0].textContent.length>0,'An empty matching dataset has a visible empty state');
});

test('removing the final task status keeps a nonempty server selection',async()=>{
  const f=fixture(); await f.init();
  const inputs=f.get('task-status-options').querySelectorAll('input');
  for(const input of inputs.filter(input=>input.checked&&input.value!=='review')){
    input.checked=false; input.handlers.change();
    f.answer(f.last(),f.list()); await flush();
  }
  const review=inputs.find(input=>input.value==='review');
  const count=f.posted.length;
  review.checked=false; review.handlers.change();
  assert.equal(review.checked,true); assert.equal(f.posted.length,count);
  assert.equal(f.last().params.arguments.status,'review');
});

const drainRead=(workspace='host-a/ws_shared',capacity={})=>({schema_version:1,workspace,scope:'drain',controls_authorized:true,capacity:{active_leaf_runs:2,max_active_leaf_runs:4,free_slots:2,...capacity},tasks:[{task_id:'ORB-1',eligible:true},{task_id:'ORB-2',eligible:false,reason:'context_lock_conflict'}]});
test('drain defaults to review, explicit completion is forwarded and stop keeps start settings out',async()=>{
  const f=fixture();await f.init();f.click('drain');
  assert.equal(f.last().params.arguments.action,'status');
  f.answer(f.last(),drainRead());await flush();
  assert.equal(f.get('drain-start').disabled,false);
  f.get('drain-duration').value='3600';f.get('drain-concurrency').value='3';f.get('drain-complete').checked=false;
  f.get('drain-form').handlers.submit({preventDefault(){}});
  const request=f.last();assert.equal(request.params.name,'orbit_workflow_auto');
  assert.deepEqual(JSON.parse(JSON.stringify(request.params.arguments)),{workspace:'host-a/ws_shared',action:'start',for_seconds:3600,concurrency:3,complete:false});
  f.answer(request,{schema_version:1,workspace:'host-a/ws_shared',action:'start',run_id:'jrun-new',state:'submitted',completion:'review'});await flush();
  f.answer(f.last(),drainRead('host-a/ws_shared',{drain_run_id:'jrun-new'}));await flush();
  assert.equal(f.get('drain-start').disabled,true);
  f.click('drain-stop');assert.deepEqual(JSON.parse(JSON.stringify(f.last().params.arguments)),{workspace:'host-a/ws_shared',action:'stop'});
});
test('drain response loss never resubmits a start and remains uncertain across workspace changes',async()=>{
  const f=fixture();await f.init();f.click('drain');f.answer(f.last(),drainRead());await flush();
  f.get('drain-duration').value='1800';f.get('drain-complete').checked=true;
  f.get('drain-form').handlers.submit({preventDefault(){}});const request=f.last();assert.equal(request.params.arguments.complete,true);
  f.receive({source:f.parent,data:{jsonrpc:'2.0',id:request.id,error:{message:'connection lost'}}});await flush();
  assert.ok(f.get('drain-feedback').textContent.includes('outcome unknown'));
  f.click('refresh');f.answer(f.last(),drainRead());await flush();
  assert.equal(f.posted.filter(m=>m.params?.name==='orbit_workflow_auto'&&m.params.arguments.action==='start').length,1);
  assert.equal(f.get('drain-start').disabled,true);
  f.get('workspace').value='host-b/ws_shared';f.get('workspace').handlers.change();f.answer(f.last(),drainRead('host-b/ws_shared'));await flush();
  assert.equal(f.get('drain-start').disabled,false);
  f.get('workspace').value='host-a/ws_shared';f.get('workspace').handlers.change();f.answer(f.last(),drainRead());await flush();
  assert.equal(f.get('drain-start').disabled,true);
});
test('late drain read cannot enable controls for a different workspace and a read failure disables controls',async()=>{
  const f=fixture();await f.init();f.click('drain');const old=f.last();
  f.get('workspace').value='host-b/ws_shared';f.get('workspace').handlers.change();const current=f.last();
  f.answer(old,drainRead());await flush();assert.equal(f.get('drain-start').disabled,true);
  f.answer(current,drainRead('host-b/ws_shared'));await flush();assert.equal(f.get('drain-start').disabled,false);
  f.click('drain-refresh');f.answer(f.last(),{message:'operator required'},true);await flush();
  assert.equal(f.get('drain-start').disabled,true);assert.equal(f.get('drain-stop').disabled,true);
});
test('a crew changed during create or edit remains an unsent draft after success',async()=>{
 for(const mode of ['create','edit']){
  const f=fixture();await f.init();
  if(mode==='edit'){
   f.click('refresh');f.answer(f.last(),f.list([{id:'ORB-1',title:'Task'}]));await flush();
   f.get('list').querySelectorAll('button')[0].handlers.click();f.answer(f.last(),f.detail());await flush();f.click('edit');
  }else f.click('create');
  f.get('draft-title').value='Submitted title';f.get('draft-description').value='Description';
  f.get('draft-criteria').value='Works';f.get('draft-priority').value='medium';f.get('draft-crew').value='sol';
  f.get('task-form').handlers.submit({preventDefault(){}});const write=f.last();
  f.get('draft-crew').value='astra';
  f.answer(write,{snapshot:f.detail('ORB-1','rev2').snapshot,replayed:false});await flush();
  assert.equal(f.get('editor').hidden,false,`${mode}: a newer crew choice must not close the editor`);
  f.click('cancel-edit');f.click(mode==='create'?'create':'edit');
  assert.equal(f.get('draft-crew').value,'astra',`${mode}: closing and reopening retains the newer crew draft`);
 }
});

test('task editor traps background interaction, Escape preserves draft and restores invoking control',async()=>{
  const f=fixture();await f.init();const invoker=new Node('button');f.document.activeElement=invoker;
  f.click('create');f.get('draft-title').value='Keep this';assert.equal(f.get('app-shell').inert,true);
  f.events.keydown({key:'Escape',preventDefault(){}});assert.equal(f.get('editor').hidden,true);assert.equal(f.get('app-shell').inert,false);assert.equal(invoker.focused,true);
  f.click('create');assert.equal(f.get('draft-title').value,'Keep this');
});

const automationRead=(scope='routines',workspace='host-a/ws_shared',items=[{name:'daily',enabled:true,state:'scheduled',target:'job:maintenance',schedule:{cron:'0 9 * * *'},toggle_available:true}])=>({schema_version:1,workspace,scope,items,total:items.length,controls_authorized:true,pagination:{next_offset:null},notes:[]});
const actionButton=(f,text)=>f.get('automation-detail').querySelectorAll('button').find(b=>b.textContent===text);
test('automation switches read scopes, toggles observed state and requires an explicit mint confirmation',async()=>{
 const f=fixture();await f.init();f.click('automation');assert.equal(f.last().params.name,'orbit_routine_control');
 f.answer(f.last(),automationRead());await flush();f.get('automation-list').children[0].handlers.click();
 actionButton(f,'Disable definition').handlers.click();const toggle=f.last();
 assert.deepEqual(JSON.parse(JSON.stringify(toggle.params.arguments)),{workspace:'host-a/ws_shared',action:'toggle',name:'daily',expected_enabled:true,enabled:false,target:'job:maintenance'});
 f.answer(toggle,{schema_version:1,workspace:'host-a/ws_shared',action:'toggle',kind:'routine',name:'daily',enabled:false});await flush();
 f.answer(f.last(),automationRead());await flush();
 f.click('automation-auto_tasks');assert.equal(f.last().params.name,'orbit_auto_task_list');
 f.answer(f.last(),automationRead('auto_tasks','host-a/ws_shared',[{name:'qa',enabled:true,mint_available:true}]));await flush();
 f.get('automation-list').children[0].handlers.click();const before=f.posted.length;
 actionButton(f,'Mint task…').handlers.click();assert.equal(f.posted.length,before,'opening confirmation has no effect');
 assert.match(f.get('automation-detail').textContent,/ignores the schedule/);
 actionButton(f,'Mint one task').handlers.click();assert.equal(f.last().params.arguments.acknowledge_unconditional,true);assert.equal(f.last().params.name,'orbit_auto_task_mint');
});
test('automation scopes unknown outcomes and never repeats a run after refresh',async()=>{
 const f=fixture();await f.init();f.click('automation');f.answer(f.last(),automationRead());await flush();
 f.click('automation-jobs');f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[{name:'maintenance',run_available:true}]));await flush();
 f.get('automation-list').children[0].handlers.click();actionButton(f,'Run job…').handlers.click();actionButton(f,'Submit job').handlers.click();const write=f.last();
 f.receive({source:f.parent,data:{jsonrpc:'2.0',id:write.id,error:{message:'lost reply'}}});await flush();
 assert.match(f.get('automation-feedback').textContent,/outcome unknown/);
 f.click('automation-refresh');f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[{name:'maintenance',run_available:true}]));await flush();
 assert.equal(actionButton(f,'Run job…').disabled,true);
 assert.equal(f.posted.filter(m=>m.params?.name==='orbit_pipeline_invoke').length,1);
 f.get('workspace').value='host-b/ws_shared';f.get('workspace').handlers.change();f.answer(f.last(),automationRead('jobs','host-b/ws_shared',[{name:'maintenance',run_available:true}]));await flush();
 f.get('automation-list').children[0].handlers.click();assert.equal(actionButton(f,'Run job…').disabled,false);
});
test('late automation reads cannot replace another destination and pagination is explicit',async()=>{
 const f=fixture();await f.init();f.click('automation');const stale=f.last();
 f.get('workspace').value='host-b/ws_shared';f.get('workspace').handlers.change();const current=f.last();
 f.answer(stale,automationRead());await flush();assert.equal(f.get('automation-list').children.length,0);
 f.answer(current,{...automationRead('routines','host-b/ws_shared'),total:30,pagination:{next_offset:25}});await flush();
 f.click('automation-next');assert.equal(f.last().params.arguments.offset,25);assert.equal(f.last().params.arguments.workspace,'host-b/ws_shared');
 f.answer(f.last(),{message:'permission denied'},true);await flush();assert.match(f.get('automation-feedback').textContent,/Automation unavailable/);
});
test('late drain receipt after leaving the view cannot refresh or overwrite current feedback',async()=>{
 const f=fixture();await f.init();f.click('drain');f.answer(f.last(),drainRead());await flush();
 f.get('drain-duration').value='3600';f.get('drain-form').handlers.submit({preventDefault(){}});const write=f.last();
 f.click('tasks');const count=f.posted.length;
 f.answer(write,{schema_version:1,workspace:'host-a/ws_shared',action:'start',run_id:'late',state:'submitted',completion:'review'});await flush();
 assert.equal(f.posted.length,count,'late write does not request hidden readiness');
 assert.equal(f.get('drain-start').disabled,true);
});
test('automation confirmation survives an observational refresh but cancels on definition change',async()=>{
 const f=fixture();await f.init();f.click('automation');f.answer(f.last(),automationRead());await flush();
 f.click('automation-jobs');const job={name:'maintenance',state:'enabled',run_available:true,steps:2};
 f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[job]));await flush();
 f.get('automation-list').children[0].handlers.click();actionButton(f,'Run job…').handlers.click();
 f.click('automation-refresh');assert.equal(actionButton(f,'Submit job').disabled,true);
 f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[job]));await flush();
 assert.equal(actionButton(f,'Submit job').disabled,false,'same definition retains confirmation');
 f.click('automation-refresh');f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[{...job,state:'disabled',run_available:false}]));await flush();
 assert.equal(actionButton(f,'Submit job'),undefined,'changed definition invalidates confirmation');
 assert.equal(f.posted.filter(m=>m.params?.name==='orbit_pipeline_invoke').length,0);
});

test('automation selection and confirmation keep keyboard focus without passive focus stealing',async()=>{
 const f=fixture();await f.init();f.click('automation');f.answer(f.last(),automationRead());await flush();
 const row=f.get('automation-list').children[0];f.document.activeElement=row;row.handlers.click();
 assert.equal(f.get('automation-list').children[0].focused,true,'selection retains focus on the replacement row');
 f.get('automation-list').children[0].handlers.click();
 assert.equal(f.get('automation-list').children[0].focused,true,'closing the selection retains row focus');
 f.click('automation-jobs');const job={name:'maintenance',state:'enabled',run_available:true,steps:2};
 f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[job]));await flush();
 f.get('automation-list').children[0].handlers.click();actionButton(f,'Run job…').handlers.click();
 assert.equal(actionButton(f,'Submit job').focused,true,'opening confirmation focuses Submit');
 actionButton(f,'Cancel').handlers.click();
 assert.equal(actionButton(f,'Run job…').focused,true,'cancel restores the action opener');
 f.document.activeElement=f.get('workspace');f.click('automation-refresh');
 f.answer(f.last(),automationRead('jobs','host-a/ws_shared',[job]));await flush();
 assert.ok(!f.get('automation-list').children[0].focused,'passive refresh does not move focus into the list');
 assert.ok(!actionButton(f,'Run job…').focused,'passive refresh does not move focus into detail actions');
});
