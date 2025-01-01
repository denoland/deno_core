type Thing = {
  name: string;
};

try {
  throw new Error("This is an error");
} catch (e) {
  Error.prepareStackTrace = (_, stack) => {
    return stack.map((s) => ({
      filename: s.getFileName(),
      methodName: s.getMethodName(),
      functionName: s.getFunctionName(),
      lineNumber: s.getLineNumber(),
      columnNumber: s.getColumnNumber(),
    }));
  };
  console.log(e.stack);
}
