use super::*;
use sha2::{Digest,Sha256};
use std::{fs::{File,OpenOptions},io::{Read,Write},path::{Path,PathBuf}};
use serde::de::{self,Deserialize,Deserializer,MapAccess,SeqAccess,Visitor};

struct ClosedJson(Value);
impl<'de> Deserialize<'de> for ClosedJson {
    fn deserialize<D:Deserializer<'de>>(d:D)->Result<Self,D::Error>{
        struct V;impl<'de> Visitor<'de> for V {type Value=ClosedJson;
            fn expecting(&self,f:&mut std::fmt::Formatter)->std::fmt::Result{f.write_str("JSON with unique object keys")}
            fn visit_bool<E:de::Error>(self,v:bool)->Result<Self::Value,E>{Ok(ClosedJson(json!(v)))}
            fn visit_i64<E:de::Error>(self,v:i64)->Result<Self::Value,E>{Ok(ClosedJson(json!(v)))}
            fn visit_u64<E:de::Error>(self,v:u64)->Result<Self::Value,E>{Ok(ClosedJson(json!(v)))}
            fn visit_f64<E:de::Error>(self,v:f64)->Result<Self::Value,E>{serde_json::Number::from_f64(v).map(|n|ClosedJson(Value::Number(n))).ok_or_else(||E::custom("non-finite JSON"))}
            fn visit_str<E:de::Error>(self,v:&str)->Result<Self::Value,E>{Ok(ClosedJson(json!(v)))}
            fn visit_string<E:de::Error>(self,v:String)->Result<Self::Value,E>{Ok(ClosedJson(json!(v)))}
            fn visit_none<E:de::Error>(self)->Result<Self::Value,E>{Ok(ClosedJson(Value::Null))}
            fn visit_unit<E:de::Error>(self)->Result<Self::Value,E>{Ok(ClosedJson(Value::Null))}
            fn visit_seq<A:SeqAccess<'de>>(self,mut a:A)->Result<Self::Value,A::Error>{let mut rows=vec![];while let Some(v)=a.next_element::<ClosedJson>()?{rows.push(v.0);}Ok(ClosedJson(Value::Array(rows)))}
            fn visit_map<A:MapAccess<'de>>(self,mut a:A)->Result<Self::Value,A::Error>{let mut map=serde_json::Map::new();while let Some(k)=a.next_key::<String>()?{if map.contains_key(&k){return Err(de::Error::custom("duplicate JSON key"));}let v=a.next_value::<ClosedJson>()?;map.insert(k,v.0);}Ok(ClosedJson(Value::Object(map)))}
        }d.deserialize_any(V)
    }
}
pub(super) fn parse(bytes:&[u8])->ApiResult<Value>{serde_json::from_slice::<ClosedJson>(bytes).map(|v|v.0).map_err(|_|fail())}
fn local_path(path:&Path,existing:bool)->ApiResult<PathBuf>{
    let s=path.to_str().ok_or_else(fail)?;if !path.is_absolute()||s.len()>4096||s.chars().any(|c|c.is_control())||path.components().any(|c|matches!(c,std::path::Component::ParentDir|std::path::Component::CurDir)){return Err(fail());}
    #[cfg(windows)] {if s.starts_with("\\\\")||s.get(2..).is_some_and(|s|s.contains(':')){return Err(fail());}}
    for a in path.ancestors(){if !a.exists(){continue;}let m=std::fs::symlink_metadata(a).map_err(|_|fail())?;if m.file_type().is_symlink(){return Err(fail());}#[cfg(windows)]{use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err(fail());}}}
    let canonical=std::fs::canonicalize(if existing{path}else{path.parent().ok_or_else(fail)?}).map_err(|_|fail())?;
    let canonical=if existing{canonical}else{canonical.join(path.file_name().ok_or_else(fail)?)};
    if !same_path(path,&canonical){return Err(fail());}Ok(path.to_owned())
}
pub(super) fn same_path(a:&Path,b:&Path)->bool {
    #[cfg(windows)] {a.to_string_lossy().trim_start_matches("\\\\?\\").eq_ignore_ascii_case(b.to_string_lossy().trim_start_matches("\\\\?\\"))}
    #[cfg(not(windows))] {a==b}
}
fn open(pin_value:&Value)->ApiResult<File>{pin(pin_value)?;let path=local_path(Path::new(text(&pin_value["path"])?),true)?;
    let mut opts=OpenOptions::new();opts.read(true);#[cfg(windows)]{use std::os::windows::fs::OpenOptionsExt;opts.share_mode(1);}
    let file=opts.open(path).map_err(|_|fail())?;if !file.metadata().map_err(|_|fail())?.is_file(){return Err(fail());}
    #[cfg(windows)] {use std::os::windows::io::AsRawHandle;use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle,BY_HANDLE_FILE_INFORMATION};let mut info:BY_HANDLE_FILE_INFORMATION=unsafe{std::mem::zeroed()};if unsafe{GetFileInformationByHandle(file.as_raw_handle() as _,&mut info)}==0||info.nNumberOfLinks!=1{return Err(fail());}}
    #[cfg(unix)]{use std::os::unix::fs::MetadataExt;if file.metadata().map_err(|_|fail())?.nlink()!=1{return Err(fail());}}
    Ok(file)
}
pub(super) struct Pins{files:Vec<File>,bytes:u64}
impl Pins {
    pub(super) fn new()->Self{Self{files:vec![],bytes:0}}
    pub(super) fn append(&mut self,mut other:Self){self.files.append(&mut other.files);self.bytes+=other.bytes;}
    pub(super) fn bytes(&mut self,p:&Value,limit:u64,retain_bytes:bool)->ApiResult<Vec<u8>> {
        let mut f=open(p)?;let size=f.metadata().map_err(|_|fail())?.len();if size>limit {return Err(fail());}
        self.bytes=self.bytes.checked_add(size).ok_or_else(fail)?;if self.bytes>2_147_483_648{return Err(fail());}
        let mut h=Sha256::new();let mut bytes=vec![];let mut buf=[0u8;65536];let mut total=0u64;
        loop{let n=f.read(&mut buf).map_err(|_|fail())?;if n==0{break;}total+=n as u64;if total>limit{return Err(fail());}h.update(&buf[..n]);if retain_bytes{bytes.extend_from_slice(&buf[..n]);}}
        if total!=size||format!("{:x}",h.finalize())!=text(&p["sha256"])?{return Err(fail());}self.files.push(f);Ok(bytes)
    }
    pub(super) fn json(&mut self,p:&Value,limit:u64)->ApiResult<Value>{parse(&self.bytes(p,limit,true)?)}
    pub(super) fn verify(&mut self,p:&Value,limit:u64)->ApiResult<()>{self.bytes(p,limit,false)?;Ok(())}
}
pub(crate) fn read_pin(p:&Value,limit:u64)->ApiResult<Value>{Pins::new().json(p,limit)}
pub(super) fn write_new(path:&Path,value:&Value,limit:u64)->ApiResult<()> {
    local_path(path,false)?;let bytes=serde_json::to_vec(value).map_err(|_|fail())?;if bytes.len() as u64>limit{return Err(fail());}
    if path.exists(){let p=json!({"path":path.to_string_lossy(),"sha256":format!("{:x}",Sha256::digest(&bytes))});let mut held=Pins::new();if held.bytes(&p,limit,true)?!=bytes{return Err(fail());}return Ok(());}
    let temporary=path.with_file_name(format!(".predecessor-result-{}.tmp",uuid::Uuid::new_v4()));
    let mut file=OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|_|fail())?;file.write_all(&bytes).and_then(|_|file.sync_all()).map_err(|_|fail())?;drop(file);
    // Same-directory, write-through atomic promotion WITHOUT replace-existing.
    // A crash while writing the temporary file leaves the admitted Q path absent.
    #[cfg(windows)] {use std::os::windows::ffi::OsStrExt;use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let from:Vec<u16>=temporary.as_os_str().encode_wide().chain(Some(0)).collect();let to:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe{MoveFileExW(from.as_ptr(),to.as_ptr(),8)}==0{return Err(fail());}Ok(())}
    #[cfg(not(windows))] {Err(fail())}
}
/// Called only by ROOT's early offline CLI branch, never by HTTP or App workers.
pub(crate) async fn run_command(args:&[String])->ApiResult<Value> {
    if args.len()!=4||!matches!(args[1].as_str(),"--capture"|"--apply"|"--reconcile"){return Err(fail());}
    let budget=Budget::new();let input_pin=json!({"path":args[2],"sha256":args[3]});pin(&input_pin)?;
    let mut held=Pins::new();let input=held.json(&input_pin,1048576)?;
    let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|fail())?;
    let mode=&args[1];if mode=="--capture" {
        exact(&input,&["schemaVersion","kind","scope","owner","package","receiptPath","limits"])?;
        if input["schemaVersion"]!=1||input["kind"]!="root-reviewed-predecessor-workspace-capture"{return Err(fail());}
        scope(&input["scope"])?;package(&input["package"],&input["owner"])?;limits(&input["limits"])?;
        evidence::verify_package(&input["package"],&mut held)?;budget.verification()?;
        let result=crate::storage::capture_predecessor(&url,&input,&budget).await?;
        write_new(Path::new(text(&input["receiptPath"])?),&result,WORKSPACE_BYTES)?;Ok(json!({"kind":"native-predecessor-capture-written","dispatchAuthorized":false,"receiptPath":input["receiptPath"],"ledgerSha256":result["ledgerSha256"]}))
    }else{
        let verified=VerifiedImport::from_file(&input_pin,&budget)?;
        let result=crate::storage::import_predecessor(&url,&verified,mode=="--reconcile",&budget).await?;
        if result["kind"]=="native-predecessor-transport-import-result"{write_new(Path::new(text(&input["receiptPath"])?),&result,1048576)?;}Ok(result)
    }
}
pub(super) fn actual_process()->ApiResult<Value> {
    #[cfg(windows)] {use windows_sys::Win32::{Foundation::FILETIME,System::Threading::{GetCurrentProcess,GetCurrentProcessId,GetProcessTimes}};let mut birth:FILETIME=unsafe{std::mem::zeroed()};let mut exit= birth;let mut kernel=birth;let mut user=birth;
        if unsafe{GetProcessTimes(GetCurrentProcess(),&mut birth,&mut exit,&mut kernel,&mut user)}==0{return Err(fail());}
        let at=((birth.dwHighDateTime as u64)<<32)|birth.dwLowDateTime as u64;Ok(json!({"pid":unsafe{GetCurrentProcessId()},"birthFileTime":at.to_string()}))}
    #[cfg(not(windows))] {Err(fail())}
}
