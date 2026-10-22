# Static site for https://mail.camelapp.si (Camel Mail). Serves on port 8080.
FROM nginx:alpine

RUN rm -rf /usr/share/nginx/html/* /etc/nginx/conf.d/default.conf
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY security-headers.conf /etc/nginx/snippets/security-headers.conf

COPY index.html 404.html favicon.ico robots.txt sitemap.xml /usr/share/nginx/html/
COPY assets  /usr/share/nginx/html/assets
COPY privacy /usr/share/nginx/html/privacy
COPY terms   /usr/share/nginx/html/terms
COPY ar      /usr/share/nginx/html/ar

EXPOSE 8080
