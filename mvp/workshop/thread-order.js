// Sort only siblings: provider parent links remain the source of tree structure.
// Unknown timestamps retain their slots; never infer a date from an HH:mm label.
export function chronologicalSiblings(messages) {
  const dated=messages.map((message,index)=>({message,index,at:Date.parse(message.createdAt)}))
    .filter(row=>Number.isFinite(row.at)).sort((a,b)=>a.at-b.at||a.index-b.index);
  let next=0;
  return messages.map(message=>Number.isFinite(Date.parse(message.createdAt))?dated[next++].message:message);
}
