Scan the codebase for files exceeding 800 lines.

Run this command from the project folder:
```
find . -type f \( -name "*.py" -o -name "*.ts" -o -name "*.tsx" -o -name "*.js" -o -name "*.yml" -o -name "*.yaml" -o -name "*.sql" \) \
  ! -path "*/.venv/*" ! -path "*/__pycache__/*" ! -path "*/data/*" ! -path "*/.git/*" \
  ! -path "*/node_modules/*" ! -path "*/.next/*" \
  -exec wc -l {} + | awk '$1 >= 800 && !/total$/' | sort -rn
```

Generate a table with: file name, file path, number of lines.

For each file over the threshold, suggest refactoring candidates — which functions, classes, components, or sections could be extracted into separate modules.

Also show files between 500-799 lines as "approaching threshold" for awareness.
