import test from 'node:test';
import assert from 'node:assert/strict';
import {validate} from '../lib/config.js';
import {connect,agent} from '../agent.js';
const c={enabled:true,gateway:'http://127.0.0.1:3425/v1',key:'test-only',model:'provider/model',models:[{id:'provider/model'}]};
test('plugin is a Magpie agent and maps supplied credentials and models',()=>{
  const previous=process.env.MAGPIE_CURSOR_BRIDGE_NO_AUTOSTART;
  process.env.MAGPIE_CURSOR_BRIDGE_NO_AUTOSTART='1';
  try {
    assert.equal(agent.id,'cursor-bridge');const out=connect({gateway:{v1:c.gateway,key:c.key},models:c.models});assert.equal(out.key,c.key);assert.deepEqual(validate({...out,model:c.model}).models,[{id:c.model,name:c.model,context:null}]);
  } finally {
    if(previous===undefined) delete process.env.MAGPIE_CURSOR_BRIDGE_NO_AUTOSTART;
    else process.env.MAGPIE_CURSOR_BRIDGE_NO_AUTOSTART=previous;
  }
});
test('disconnected configuration is inactive',()=>{assert.equal(validate({}),null);assert.equal(validate({...c,enabled:false}),null);});
test('refuses remote gateway and credential-bearing URL',()=>{for(const gateway of ['https://evil.example/v1','http://127.0.0.1.evil.example','http://key@127.0.0.1','file:///tmp/test'])assert.throws(()=>validate({...c,gateway}));});
test('selected model must exist and IDs must be unique',()=>{assert.throws(()=>validate({...c,model:'missing'}));assert.throws(()=>validate({...c,models:[...c.models,...c.models]}));});
