// Isolated rendered acceptance fixture. No Orbit process or user state is contacted.
// node crates/orbit-mcp/src/adapter/tests/panel-preview.mjs [port]
import http from 'node:http';
import fs from 'node:fs';
import {fileURLToPath} from 'node:url';
const asset=new URL('../../../assets/task-panel/',import.meta.url);
const dashboard=new URL('../../../../orbit-web/assets/dashboard/',import.meta.url);
const read=(base,path)=>fs.readFileSync(new URL(path,base),'utf8');
export function resource(){
  return read(asset,'index.html').replace('/* ORBIT_PANEL_STYLE */',read(asset,'css/task-panel.css')).replace('/* ORBIT_PANEL_SCRIPT */',[
    read(dashboard,'vendor/marked.umd.js'),read(dashboard,'vendor/purify.min.js'),...['presentation.js','drain.js','automation.js','task-panel.js'].map(f=>read(asset,'js/'+f))
  ].join('\n'));
}
const server=http.createServer((req,res)=>{
  const url=new URL(req.url,'http://localhost');
  if(url.pathname==='/panel'){res.setHeader('content-type','text/html');res.end(resource());return;}
  if(url.pathname==='/fixture.js'){res.setHeader('content-type','text/javascript');res.end(read(new URL('.',import.meta.url),'panel-fixture.js'));return;}
  if(url.pathname==='/'){
    res.setHeader('content-type','text/html');res.end(`<!doctype html><html><head><title>Orbit UI acceptance fixture</title><style>body{margin:0;background:#24242a;color:#fff;font:12px system-ui}header{padding:8px;display:flex;gap:8px;align-items:center}button{padding:5px 10px}iframe{display:block;border:0;margin:auto;background:#0a0a0b;height:calc(100vh - 42px);width:100%;max-width:1440px}</style></head><body><header>Isolated fixture <button data-width="1440">Desktop</button><button data-width="880">Panel</button><button data-width="560">Narrow</button><button data-width="375">Phone</button><button id="theme">Toggle theme</button><span id="test-result"></span></header><iframe id="app" title="Orbit Control Center" src="/panel"></iframe><script src="/fixture.js"></script></body></html>`);return;
  }
  res.writeHead(404);res.end();
});
if(process.argv[1]===fileURLToPath(import.meta.url))server.listen(Number(process.argv[2]||4318),'127.0.0.1',()=>console.log('Orbit isolated UI fixture: http://127.0.0.1:'+server.address().port));
