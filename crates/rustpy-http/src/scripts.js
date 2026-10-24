'use strict';
const vm=require('node:vm');
const fs=require('node:fs');
try {
 const input=JSON.parse(fs.readFileSync(0,'utf8'));
 const globals={...input.globals},variables={...input.variables},tests=[],logs=[];
 const store=values=>({get:key=>values[key],set:(key,value)=>{values[key]=String(value)},clear:key=>{delete values[key]},clearAll:()=>{for(const key of Object.keys(values))delete values[key]}});
 const assert=(value,message='Assertion failed')=>{if(!value)throw new Error(message)};
 const response=input.response||{}; const pairs=response.headers||[];
 response.headers={valueOf:name=>pairs.find(([key])=>key.toLowerCase()===name.toLowerCase())?.[1],valuesOf:name=>pairs.filter(([key])=>key.toLowerCase()===name.toLowerCase()).map(([,value])=>value)};
 const client={
  global:store(globals),
  assert,
  log:value=>{if(logs.length<1000)logs.push(String(value))},
  test:(name,fn)=>{
   try{
    fn();
    tests.push({name:String(name),passed:true,message:''});
   }catch(error){
    tests.push({name:String(name),passed:false,message:String(error.message)});
   }
  }
 };
 const context=vm.createContext({client,response,request:{variables:store(variables)},console:{log:client.log}},{codeGeneration:{strings:false,wasm:false}});
 new vm.Script(input.source,{filename:'http-request-script.js'}).runInContext(context,{timeout:1000});
 process.stdout.write(JSON.stringify({globals,variables,tests,logs}));
}catch(error){process.stderr.write(String(error.stack||error));process.exitCode=1}
