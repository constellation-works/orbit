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
const source=process.env.ORBIT_PANEL_RESOURCE?readFileSync(process.env.ORBIT_PANEL_RESOURCE,'utf8').match(/<script>([\s\S]*?)<\/script>/)?.[1]:readFileSync(new URL('../../../assets/task-panel/js/task-panel.js',import.meta.url),'utf8');
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
    this.style={
    };
    this.classList={
      add(){
      },remove(){
      }
    };
  }
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
        structuredContent:data,isError
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
  assert.equal(f.get('list').children.length,50);
  assert.ok(performance.now()-start<1000);
  f.get('list').children[1].handlers.click();
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
  assert.equal(f.get('list').children[0].textContent,'ORB-1 · Good');
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
  f.get('list').children[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('comment').value='My preserved comment';
  f.get('comment-form').handlers.submit({
    preventDefault(){
    }
  });
  const write=f.last();
  assert.equal(write.params.name,'orbit_desktop_task_write');
  assert.equal(write.params.arguments.operation.expected_revision,'rev1');
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
  f.get('list').children[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('evidence').value='artifact:checks';
  f.get('rationale').value='Fix missing test';
  f.click('changes');
  const op=f.last().params.arguments.operation;
  assert.equal(op.kind,'review');
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
  assert.equal(f.get('list').children.length,50);
  f.get('list').children[0].handlers.click();
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
  assert.equal(create.params.arguments.operation.kind,'create');
  assert.equal(create.params.arguments.operation.priority,'high');
  assert.deepEqual([...create.params.arguments.operation.acceptance_criteria],['Criterion one','Criterion two']);
  f.answer(create,{
    snapshot:f.detail().snapshot,replayed:false
  });
  await flush();
  const readRequests=f.posted.filter(m=>m.params?.name==='orbit_desktop_read').slice(-2);
  for(const request of readRequests)f.answer(request,request.params.arguments.scope==='task'?f.detail():f.list());
  await flush();
  f.click('edit');
  f.get('draft-title').value='Changed title';
  f.get('task-form').handlers.submit({
    preventDefault(){
    }
  });
  const edit=f.last().params.arguments.operation;
  assert.equal(edit.kind,'edit');
  assert.equal(edit.expected_revision,'rev1');
  assert.equal(edit.fields.title,'Changed title');
  assert.ok(!('title' in edit));
});
test('server action refusal disables completion; generic errors retain retry identity and draft',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').children[0].handlers.click();
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
  assert.equal(f.last().params.name,'orbit_desktop_task_write');
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
  f.get('list').children[0].handlers.click();
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
  const pending=f.posted.filter(m=>m.params?.arguments?.scope==='runs').at(-1);
  f.answer(pending,f.list([{
    id:'jrun-1',state:'running'
  }]));
  await flush();
  f.get('list').children[0].handlers.click();
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
  f.get('status').value='review';
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
  assert.equal(detailRequest.params.arguments.scope,'run');
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
  f.get('list').children[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-title').value='Keep this edit';
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Reviewed original evidence';
  f.click('refresh');
  const requests=f.posted.filter(m=>m.params?.name==='orbit_desktop_read').slice(-2);
  for(const request of requests)f.answer(request,request.params.arguments.scope==='task'?f.detail('ORB-1','rev2'):f.list());
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
  assert.equal(f.get('list').children[0].textContent,'ORB-1 · Good');
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
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
  const d=f.detail();
  d.snapshot.reviewed_head='head-a';
  d.snapshot.task.job_run_id='jrun-evidence';
  f.answer(f.last(),d);
  await flush();
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Checked';
  f.click('changes');
  const verdict=f.last().params.arguments.operation.verdict;
  assert.equal(verdict.expected_head,'head-a');
  assert.equal(verdict.expected_run_id,'jrun-evidence');
  f.answer(f.last(),{
    workspace:'host-a/ws_shared',conflict:{
      code:'revision_conflict',message:'Evidence changed'
    },snapshot:d.snapshot
  });
  await flush();
  f.click('refresh');
  const requests=f.posted.filter(m=>m.params?.name==='orbit_desktop_read').slice(-2);
  const changed=f.detail();
  changed.snapshot.reviewed_head='head-b';
  for(const request of requests)f.answer(request,request.params.arguments.scope==='task'?changed:f.list());
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
  f.get('list').children[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.click('edit');
  f.get('draft-title').value='First entity draft';
  f.get('evidence').value='execution_summary';
  f.get('rationale').value='Checked first';
  f.get('criterion-outcomes').querySelectorAll('select')[0].value='met';
  f.get('list').children[1].handlers.click();
  f.answer(f.last(),f.detail('ORB-2','second-revision'));
  await flush();
  assert.equal(f.get('editor').hidden,true);
  assert.equal(f.get('evidence').value,'');
  f.click('edit');
  assert.notEqual(f.get('draft-title').value,'First entity draft');
  f.get('draft-title').value='Second entity draft';
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  assert.ok(f.get('identity').textContent.includes('fresh-revision'));
  assert.ok(f.get('state').textContent.includes('Write refused'));
  f.click('refresh');
  assert.equal(f.last().params.name,'orbit_desktop_read');
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
  f.get('list').children[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  f.get('comment').value='Submitted';
  f.get('comment-form').handlers.submit({
    preventDefault(){
    }
  });
  const write=f.last();
  f.get('list').children[1].handlers.click();
  f.answer(f.last(),f.detail('ORB-2','rev2'));
  await flush();
  f.get('comment').value='Second task draft';
  f.answer(write,{
    snapshot:f.detail().snapshot,replayed:false
  });
  await flush();
  assert.ok(f.get('identity').textContent.includes('ORB-2'));
  assert.equal(f.get('comment').value,'Second task draft');
  const requests=f.posted.filter(m=>m.params?.name==='orbit_desktop_read').slice(-2);
  for(const request of requests)f.answer(request,request.params.arguments.scope==='task'?f.detail('ORB-2','rev2'):f.list([{
    id:'ORB-1',title:'One'
  },{
    id:'ORB-2',title:'Two'
  }]));
  await flush();
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  assert.ok(f.get('details').children.some(n=>n.children[1].textContent.includes('stdout_truncated')));
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
  f.get('list').children[0].handlers.click();
  f.answer(f.last(),f.detail());
  await flush();
  const criterion=f.get('criterion-outcomes').querySelectorAll('select')[0];
  f.document.activeElement=f.get('list').children[0];
  f.click('refresh');
  const requests=f.posted.filter(m=>m.params?.name==='orbit_desktop_read').slice(-2);
  for(const request of requests)f.answer(request,request.params.arguments.scope==='task'?f.detail():f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  assert.equal(f.get('criterion-outcomes').querySelectorAll('select')[0],criterion);
  assert.equal(f.get('list').children[0].focused,true);
});
test('chat context bounds task text and references while retaining authoritative entity identity',async()=>{
  const f=fixture();
  await f.init();
  f.click('refresh');
  f.answer(f.last(),f.list([{
    id:'ORB-1',title:'Task'
  }]));
  await flush();
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  const op=f.last().params.arguments.operation;
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
  const summary=f.get('list').children[0].children[0].textContent;
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
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  f.get('list').children[0].handlers.click();
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
  assert.equal(corrected.params.arguments.operation.fields.crew,'crew_sol');
  assert.equal(corrected.params.arguments.operation.expected_revision,'rev1');
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
