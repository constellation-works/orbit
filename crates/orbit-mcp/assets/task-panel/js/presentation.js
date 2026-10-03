/** Presentation only. The MCP controller owns routing, freshness and authority. */
window.OrbitPanelView = (() => {
  'use strict';
  const node = (tag, text, className) => {
    const n=document.createElement(tag);
    if(text!=null)n.textContent=String(text);
    if(className)n.className=className;
    return n;
  };
  const label = value => String(value??'Unknown').replaceAll('_',' ').replaceAll('-',' ');
  const time = value => {
    const date=new Date(value);
    return value&&Number.isFinite(date.getTime())?date.toLocaleString(undefined,{month:'short',day:'numeric',hour:'numeric',minute:'2-digit'}):'Time unavailable';
  };
  const duration = value => {
    if(value==null||!Number.isFinite(Number(value)))return 'Duration unavailable';
    const seconds=Math.round(Number(value)/1000);
    return seconds<60?`${seconds}s`:seconds<3600?`${Math.floor(seconds/60)}m ${seconds%60}s`:`${Math.floor(seconds/3600)}h ${Math.floor(seconds%3600/60)}m`;
  };
  const badge = value => {
    const n=node('span',label(value),'badge');
    n.dataset.status=String(value||'unknown').replaceAll('_','-');
    return n;
  };
  let configured=false;
  function markdown(target, value){
    const original=String(value??'');
    const text=original.slice(0,64000)+(original.length>64000?'\n[Display truncated at 64,000 characters]':'');
    if(!window.marked||!window.DOMPurify?.isSupported){target.textContent=text;return;}
    if(!configured){
      window.marked.use({renderer:{html(token){
        return String(token.raw??token).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;');
      }}});
      configured=true;
    }
    // Reuse the dashboard's pinned parser/purifier; no raw HTML, images, forms,
    // external resources or executable URLs can escape into the host document.
    const fragment=window.DOMPurify.sanitize(window.marked.parse(text,{async:false}),{
      RETURN_DOM_FRAGMENT:true, USE_PROFILES:{html:true},
      FORBID_TAGS:['style','img','form','input','button','textarea','select','iframe'],
      FORBID_ATTR:['style','id','name','target'],
    });
    for(const a of fragment.querySelectorAll('a')){
      const href=a.getAttribute('href')||'';
      if(!/^https?:\/\//i.test(href)){a.removeAttribute('href');continue;}
      a.setAttribute('target','_blank');a.setAttribute('rel','noopener noreferrer');
    }
    target.append(fragment);
    target.classList.add('markdown');
  }
  function structured(target,value,depth=0){
    if(value==null){target.append(node('span','Not available','muted'));return;}
    if(typeof value!=='object'){markdown(target,String(value));return;}
    if(depth>4){target.append(node('pre',JSON.stringify(value,null,2),'raw-data'));return;}
    if(Array.isArray(value)){
      if(!value.length){target.append(node('span','None','muted'));return;}
      const list=node('ul',null,'value-list');
      for(const item of value.slice(0,50)){const li=node('li');structured(li,item,depth+1);list.append(li);}
      if(value.length>50)list.append(node('li',`Showing 50 of ${value.length}`));
      target.append(list);return;
    }
    const list=node('dl',null,'property-list');
    for(const [key,v]of Object.entries(value)){
      if(v==null)continue;
      list.append(node('dt',label(key)));const dd=node('dd');
      if(typeof v==='string'&&/(_at|^ts$)/.test(key)&&Number.isFinite(Date.parse(v))){dd.textContent=time(v);dd.title=v;}
      else if(key==='duration_ms'&&typeof v==='number')dd.textContent=duration(v);
      else if((key==='state'||key==='status')&&typeof v==='string')dd.append(badge(v));
      else structured(dd,v,depth+1);
      list.append(dd);
    }
    target.append(list);
  }
  function field(container,title,value,{technical=false}={}){
    const box=node(technical?'details':'section',null,technical?'field technical':'field');
    const h=node(technical?'summary':'h3',title);const body=node('div',null,'field-body');
    structured(body,value);
    box.append(h,body);container.append(box);
  }
  function row(button,row,kind,id){
    button.replaceChildren();
    const title=node('div',kind==='run'?(row.job_id||row.job_name||'Workflow run'):(row.title||'Untitled'),'entity-title');
    const meta=node('div',null,'entity-meta');
    const identity=node('span',id,'entity-key');
    const date=node('span',time(row.updated_at||row.created_at||row.started_at||row.scheduled_at),'entity-time');
    date.title=row.updated_at||row.created_at||row.started_at||row.scheduled_at||'';
    meta.append(identity,badge(row.status||row.state));
    if(kind==='run')meta.append(node('span',duration(row.duration_ms)),node('span',row.attempt==null?'Attempt unavailable':`Attempt ${row.attempt}`));
    else{
      if(row.priority)meta.append(node('span',label(row.priority),'priority'));
      if(row.crew)meta.append(node('span',row.crew));
      const count=(row.blockers||row.dependencies||[]).length;
      if(count)meta.append(node('span',`${count} ${count===1?'dependency':'dependencies'}`));
    }
    const disclosures=[];
    if(row.title_truncated)disclosures.push('title');
    if(row.crew_truncated)disclosures.push('crew');
    if(row.relations_truncated)disclosures.push(`relations (${row.relations?.length??50} of ${row.relations_total??'unknown'})`);
    if(row.dependencies_truncated)disclosures.push(`dependencies (${row.dependencies?.length??50} of ${row.dependencies_total??'unknown'})`);
    if(disclosures.length)meta.append(node('span',`Truncated: ${disclosures.join(', ')}`,'projection-note'));
    if(row.job_run_id_omitted)meta.append(node('span','Run reference omitted','projection-note'));
    button.append(title,meta,date);
  }
  return {node,label,time,duration,badge,markdown,structured,field,row};
})();
