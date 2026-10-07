import {reject} from './config.mjs';

// A finite worker owns these objects. No credential values or HTTP responses are
// cached: each transport request still checks the OS token/lifecycle generation.
export class ProviderResources {
  constructor(account,options={}) {this.account=account;this.options=options;this.closed=false;this.identity=null;this.initializing=null;}
  assertBinding(config,paths) {
    if(this.closed)reject('PROVIDER_SESSION_CLOSED');
    if(config.scope?.stableAccountKey!==this.account||paths.accountKey!==this.account)reject('ACCOUNT_SCOPE_MISMATCH');
    const identity=JSON.stringify({config,providerRepo:paths.providerRepo,configFile:paths.configFile,cardFile:paths.cardFile});
    if(this.identity!==null&&identity!==this.identity)reject('PROVIDER_SESSION_SOURCE_CHANGED');
    this.identity=identity;
  }
  async get(config,paths,moduleLoader) {
    this.assertBinding(config,paths);
    if(!this.initializing)this.initializing=this.initialize(config,moduleLoader);
    return this.initializing;
  }
  async initialize(config,moduleLoader) {
    const platform=this.options.platform??process.platform;
    const [parser,runtime,provider,gateway,credentials]=await Promise.all([
      moduleLoader('transport/portable-provider-config.ts'),moduleLoader('transport/credential-runtime.ts'),
      moduleLoader('provider/read-only-provider.ts'),moduleLoader('transport/fast-conveyor-gateway.ts'),
      moduleLoader(platform==='win32'?'transport/windows-credential-manager-store.ts':'transport/platform-credential-store.ts'),
    ]);
    const native=parser.parsePortableProviderConfig(config,platform);
    const additionalReferences=[native.oauthClientReference,native.signingKeyReference,native.replyIdentityDigestReference,native.authRecovery?.credentialReference,
      ...Object.values(native.replyIdentityDigestReferencesByObject??{})].filter(Boolean);
    let linuxSecretStoreTransport=this.options.linuxSecretStoreTransport;
    if(platform==='linux'&&!linuxSecretStoreTransport&&!this.options.credentialStoreFactory){
      const socketPath=(this.options.env??process.env).COMMUNITYHERO_LINUX_SECRET_STORE_SOCKET;
      if(typeof socketPath!=='string'||!socketPath)reject('LINUX_CREDENTIAL_STORE_UNCONFIGURED');
      const socketModule=await moduleLoader('transport/linux-secret-store-socket.ts');
      linuxSecretStoreTransport=await socketModule.createLinuxSecretStoreSocketTransport({socketPath,stableAccountKey:this.account});
    }
    let credentialStore;
    try{credentialStore=this.options.credentialStoreFactory
      ?await this.options.credentialStoreFactory(native)
      :platform==='win32'
        ?credentials.createWindowsCredentialManagerSession({tokenReference:native.tokenReference,additionalReferences,maxLifetimeMs:1200000,maxRequests:16384})
        :credentials.createPlatformCredentialStore({platform,stableAccountKey:this.account,tokenReference:native.tokenReference,
          oauthClientReference:native.oauthClientReference,additionalReferences,linuxSecretStoreTransport});
    }catch(error){await linuxSecretStoreTransport?.close?.();throw error;}
    this.store=credentialStore;
    if(this.closed){await credentialStore.close?.();reject('PROVIDER_SESSION_CLOSED');}
    const runtimeFactories=new Map(),readers=new Map(),objectIds=native.accountObjectIds??[native.scope.objectId];
    const createRuntime=options=>{
      if(this.closed)reject('PROVIDER_SESSION_CLOSED');
      if(options.credentialStore!==credentialStore||options.scope.stableAccountKey!==this.account||!objectIds.includes(options.scope.objectId))reject('ACCOUNT_SCOPE_MISMATCH');
      const key=JSON.stringify({scope:options.scope,tokenReference:options.tokenReference,oauthClientReference:options.oauthClientReference,
        lockDirectory:options.lockDirectory,executionMode:options.executionMode,replyIdentityDigest:options.replyIdentityDigest??null});
      if(!runtimeFactories.has(key)){
        // Reply identities may rotate. Bound memory without weakening an identity
        // check or retaining a transport with a different account/source binding.
        if(runtimeFactories.size>=128)runtimeFactories.delete(runtimeFactories.keys().next().value);
        runtimeFactories.set(key,runtime.createCredentialBackedAngrySpaceRuntimeFactory(options));
      }
      // Each action keeps its own in-flight/consumed-plan guard. Sharing an
      // executor falsely rejects distinct concurrent targets on the same object.
      return runtimeFactories.get(key)();
    };
    const reader=objectId=>{
      if(!objectIds.includes(objectId))reject('ACCOUNT_SCOPE_MISMATCH');
      if(!readers.has(objectId)){
        const scope={stableAccountKey:this.account,objectId};
        const rt=createRuntime({scope,credentialStore,tokenReference:native.tokenReference,oauthClientReference:native.oauthClientReference,
          lockDirectory:native.lockDirectory,executionMode:{mode:'disabled'},...(native.authRecovery?{authRecovery:native.authRecovery,accountObjectIds:objectIds}:{})});
        readers.set(objectId,new provider.AngrySpaceReadOnlyProvider(rt.transport,scope));
      }
      return readers.get(objectId);
    };
    let catalogueFlight=null;
    const catalogue=()=>{
      // Coalesce only simultaneously pending reads. Never reuse an earlier
      // successful authorization snapshot across later requests or generations.
      if(!catalogueFlight)catalogueFlight=Promise.resolve().then(()=>reader(native.scope.objectId).listAuthorizedObjects())
        .finally(()=>{catalogueFlight=null;});
      return catalogueFlight;
    };
    // Explicit local observation only. Normal reads keep their existing auth
    // lifecycle; this optional inspection never asks the provider catalogue.
    const inspectAuthStatus=async()=>{
      const rt=createRuntime({scope:native.scope,credentialStore,tokenReference:native.tokenReference,
        oauthClientReference:native.oauthClientReference,lockDirectory:native.lockDirectory,executionMode:{mode:'disabled'},
        ...(native.authRecovery?{authRecovery:native.authRecovery,accountObjectIds:objectIds}:{})});
      if(typeof rt.auth?.inspectStatus!=='function')reject('AUTH_STATUS_UNSUPPORTED');
      const status=await rt.auth.inspectStatus();
      if(status?.scopeKey!==this.account)reject('ACCOUNT_SCOPE_MISMATCH');
      return status;
    };
    return {native,credentialStore,provider,gateway,reader,catalogue,createRuntime,inspectAuthStatus};
  }
  async close() {
    if(this.closed)return;
    this.closed=true;
    if(this.initializing)await this.initializing.catch(()=>{});
    await this.store?.close?.();
  }
}
