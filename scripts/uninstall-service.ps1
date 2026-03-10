$ErrorActionPreference = "Stop"

sc.exe stop tasktui-service
sc.exe delete tasktui-service
